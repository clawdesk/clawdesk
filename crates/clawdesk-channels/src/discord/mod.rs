//! Discord Bot channel implementation.
//!
//! Connects to Discord via the Gateway WebSocket API (v10) for receiving
//! messages, and uses the REST API for sending. Implements `Channel` +
//! `Streaming` + `Reactions` + `Threaded`.
//!
//! ## Architecture
//!
//! Discord's Gateway uses a WebSocket connection with:
//! 1. Identify handshake (opcode 2)
//! 2. Heartbeat loop (opcode 1) at the interval specified by HELLO
//! 3. Dispatch events (opcode 0) — MESSAGE_CREATE, INTERACTION_CREATE, etc.
//! 4. Resume on disconnect (opcode 6)
//!
//! ## Modules
//!
//! - `interactions` — Application Commands, Message Components, Modals
//!
//! ## Rate limits
//!
//! Discord enforces per-route rate limits via response headers:
//! - X-RateLimit-Remaining
//! - X-RateLimit-Reset-After
//! - Global rate limit: 50 requests/second
//! - Message content: 2000 chars

pub mod interactions;

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, Reactions, StreamHandle, Streaming, Threaded};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, error, info, warn};

const DISCORD_API_BASE: &str = "https://discord.com/api/v10";

/// Discord's maximum message length for regular messages.
///
/// Discord rejects longer payloads with `50035 Invalid Form Body`.
const DISCORD_MAX_MESSAGE_LENGTH: usize = 2000;

/// Emoji pool for random ACK reactions on inbound messages.
const DISCORD_ACK_REACTIONS: &[&str] = &["⚡️", "🦀", "🙌", "💪", "👌", "👀", "👣"];

/// Base64 alphabet used by Discord bot tokens.
const BASE64_ALPHABET: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Discord Bot channel.
pub struct DiscordChannel {
    client: Client,
    bot_token: String,
    application_id: String,
    /// Allowed guild IDs. Empty = allow all guilds (DMs always allowed).
    allowed_guild_ids: Vec<u64>,
    /// Allowed user IDs. `"*"` = allow everyone.
    allowed_users: Vec<String>,
    /// Whether to process messages from other bots.
    listen_to_bots: bool,
    /// When true, only respond to messages that @mention the bot.
    mention_only: bool,
    /// Explicit default channel ID for cross-channel sends.
    /// If set, `default_origin()` uses this instead of requiring an inbound message first.
    default_channel_id: Option<u64>,
    /// Auto-discovered channel ID from the first inbound message.
    /// Populated lazily when no `default_channel_id` is configured.
    /// Used by `default_origin()` as fallback — enables cross-channel sends
    /// once any Discord message has been received.
    discovered_channel_id: AtomicU64,
    /// Auto-discovered guild ID from the first inbound message.
    discovered_guild_id: AtomicU64,
    /// Shutdown flag.
    running: AtomicBool,
    shutdown: Notify,
    /// Active typing indicator handles.
    typing_handles: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
}

impl DiscordChannel {
    pub fn new(
        bot_token: String,
        application_id: String,
        allowed_guild_ids: Vec<u64>,
        allowed_users: Vec<String>,
        listen_to_bots: bool,
        mention_only: bool,
        default_channel_id: Option<u64>,
    ) -> Self {
        if let Some(ch_id) = default_channel_id {
            info!(default_channel_id = ch_id, "Discord: explicit default channel configured for cross-channel sends");
        }
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            bot_token,
            application_id,
            allowed_guild_ids,
            allowed_users,
            listen_to_bots,
            mention_only,
            default_channel_id,
            discovered_channel_id: AtomicU64::new(0),
            discovered_guild_id: AtomicU64::new(0),
            running: AtomicBool::new(false),
            shutdown: Notify::new(),
            typing_handles: Mutex::new(HashMap::new()),
        }
    }

    /// Check if a guild is allowed.
    fn is_allowed_guild(&self, guild_id: u64) -> bool {
        self.allowed_guild_ids.is_empty() || self.allowed_guild_ids.contains(&guild_id)
    }

    /// Check if a Discord user ID is in the allowlist.
    /// Empty list means deny everyone until explicitly configured.
    /// `"*"` means allow everyone.
    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    /// Discord REST API URL.
    fn api_url(&self, path: &str) -> String {
        format!("{DISCORD_API_BASE}{path}")
    }

    /// Extract the bot's own user ID from the token.
    /// Discord bot tokens are base64(bot_user_id).timestamp.hmac
    fn bot_user_id_from_token(token: &str) -> Option<String> {
        let part = token.split('.').next()?;
        base64_decode(part)
    }

    /// Normalize a Discord MESSAGE_CREATE event to NormalizedMessage.
    fn normalize_event(
        &self,
        event: &DiscordMessageCreate,
        bot_user_id: &str,
    ) -> Option<NormalizedMessage> {
        // Skip bot messages (unless listen_to_bots is enabled)
        if !self.listen_to_bots && event.author.bot.unwrap_or(false) {
            debug!(author = %event.author.username, "Discord: skipping bot message");
            return None;
        }

        // Skip messages from the bot itself
        if event.author.id == bot_user_id {
            debug!("Discord: skipping own message");
            return None;
        }

        // User allowlist check
        if !self.is_user_allowed(&event.author.id) {
            warn!(
                user_id = %event.author.id,
                username = %event.author.username,
                allowed_users = ?self.allowed_users,
                "Discord: ignoring message from unauthorized user"
            );
            return None;
        }

        // Mention-only filtering and content normalization
        let content = normalize_incoming_content(
            &event.content,
            self.mention_only,
            bot_user_id,
        )?;

        let guild_id: u64 = event
            .guild_id
            .as_ref()
            .and_then(|g| g.parse().ok())
            .unwrap_or(0);
        let channel_id: u64 = event.channel_id.parse().unwrap_or(0);
        let message_id: u64 = event.id.parse().unwrap_or(0);
        let is_dm = guild_id == 0;

        // Guild filter (DMs always pass through)
        if !is_dm && !self.is_allowed_guild(guild_id) {
            return None;
        }

        let sender = SenderIdentity {
            id: event.author.id.clone(),
            display_name: event
                .member
                .as_ref()
                .and_then(|m| m.nick.clone())
                .unwrap_or_else(|| event.author.username.clone()),
            channel: ChannelId::Discord,
        };

        let session_key = if is_dm {
            clawdesk_types::session::SessionKey::new(ChannelId::Discord, &event.author.id)
        } else {
            clawdesk_types::session::SessionKey::new(
                ChannelId::Discord,
                &format!("{guild_id}:{channel_id}"),
            )
        };

        let origin = clawdesk_types::message::MessageOrigin::Discord {
            guild_id,
            channel_id,
            message_id,
            is_dm,
            thread_id: None,
        };

        Some(NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key,
            body: content,
            body_for_agent: None,
            sender,
            media: vec![],
            artifact_refs: vec![],
            reply_context: None,
            origin,
            timestamp: chrono::Utc::now(),
        })
    }

    /// Trigger the typing indicator for a Discord channel.
    pub async fn start_typing(&self, channel_id: &str) -> Result<(), String> {
        self.stop_typing(channel_id).await?;

        let client = self.client.clone();
        let token = self.bot_token.clone();
        let cid = channel_id.to_string();

        let handle = tokio::spawn(async move {
            let url = format!("{DISCORD_API_BASE}/channels/{cid}/typing");
            loop {
                let _ = client
                    .post(&url)
                    .header("Authorization", format!("Bot {token}"))
                    .send()
                    .await;
                // Discord typing indicator lasts ~10s, re-trigger every 8s
                tokio::time::sleep(Duration::from_secs(8)).await;
            }
        });

        if let Ok(mut guard) = self.typing_handles.lock() {
            guard.insert(channel_id.to_string(), handle);
        }
        Ok(())
    }

    /// Cancel the typing indicator for a Discord channel.
    pub async fn stop_typing(&self, channel_id: &str) -> Result<(), String> {
        if let Ok(mut guard) = self.typing_handles.lock() {
            if let Some(handle) = guard.remove(channel_id) {
                handle.abort();
            }
        }
        Ok(())
    }

    /// Connect to Discord Gateway WebSocket and dispatch inbound messages.
    ///
    /// This method drives the full Gateway lifecycle:
    /// 1. Fetch gateway URL from /gateway/bot
    /// 2. Connect WebSocket and perform HELLO + IDENTIFY handshake
    /// 3. Run heartbeat timer and dispatch MESSAGE_CREATE to the sink
    /// 4. Handle Reconnect (op 7) and Invalid Session (op 9)
    ///
    /// The caller should spawn this on a tokio task and handle reconnection
    /// on return.
    pub async fn gateway_loop(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        let bot_user_id = Self::bot_user_id_from_token(&self.bot_token).unwrap_or_default();

        // Get Gateway URL
        let gw_resp: serde_json::Value = self
            .client
            .get(&self.api_url("/gateway/bot"))
            .header("Authorization", format!("Bot {}", self.bot_token))
            .send()
            .await
            .map_err(|e| format!("Discord gateway fetch failed: {e}"))?
            .json()
            .await
            .map_err(|e| format!("Discord gateway parse failed: {e}"))?;

        let gw_url = gw_resp
            .get("url")
            .and_then(|u| u.as_str())
            .unwrap_or("wss://gateway.discord.gg");

        let ws_url = format!("{gw_url}/?v=10&encoding=json");
        info!("Discord: connecting to gateway...");

        let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .map_err(|e| format!("Discord WebSocket connect failed: {e}"))?;

        let (mut write, mut read) = ws_stream.split();

        // Read Hello (opcode 10)
        let hello = read
            .next()
            .await
            .ok_or_else(|| "Discord: no Hello from gateway".to_string())?
            .map_err(|e| format!("Discord: Hello read error: {e}"))?;

        let hello_data: serde_json::Value =
            serde_json::from_str(&hello.to_string())
                .map_err(|e| format!("Discord: Hello parse error: {e}"))?;

        let heartbeat_interval = hello_data
            .get("d")
            .and_then(|d| d.get("heartbeat_interval"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(41250);

        // Send Identify (opcode 2)
        let identify = json!({
            "op": 2,
            "d": {
                "token": self.bot_token,
                "intents": 37377, // GUILDS | GUILD_MESSAGES | MESSAGE_CONTENT | DIRECT_MESSAGES
                "properties": {
                    "os": std::env::consts::OS,
                    "browser": "clawdesk",
                    "device": "clawdesk"
                }
            }
        });
        write
            .send(WsMessage::Text(identify.to_string().into()))
            .await
            .map_err(|e| format!("Discord Identify send failed: {e}"))?;

        info!("Discord: connected and identified");

        // Track the last sequence number for heartbeats and resume
        let mut sequence: i64 = -1;

        // Spawn heartbeat timer
        let (hb_tx, mut hb_rx) = tokio::sync::mpsc::channel::<()>(1);
        let hb_ms = heartbeat_interval;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(hb_ms));
            loop {
                interval.tick().await;
                if hb_tx.send(()).await.is_err() {
                    break;
                }
            }
        });

        // Main dispatch loop
        while self.running.load(Ordering::Relaxed) {
            tokio::select! {
                _ = hb_rx.recv() => {
                    let d = if sequence >= 0 { json!(sequence) } else { json!(null) };
                    let hb = json!({"op": 1, "d": d});
                    if write.send(WsMessage::Text(hb.to_string().into())).await.is_err() {
                        warn!("Discord: heartbeat send failed — breaking loop");
                        break;
                    }
                }
                msg = read.next() => {
                    let msg = match msg {
                        Some(Ok(WsMessage::Text(t))) => t,
                        Some(Ok(WsMessage::Close(frame))) => {
                            let code = frame.as_ref().map(|f| f.code);
                            let reason = frame.as_ref()
                                .map(|f| f.reason.to_string())
                                .unwrap_or_default();
                            warn!(
                                ?code,
                                reason = %reason,
                                "Discord: received WebSocket Close frame"
                            );
                            // Fatal close codes — no point retrying
                            use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
                            let is_fatal = matches!(
                                code,
                                Some(CloseCode::Library(4004))  // Authentication failed
                                | Some(CloseCode::Library(4010))  // Invalid shard
                                | Some(CloseCode::Library(4011))  // Sharding required
                                | Some(CloseCode::Library(4012))  // Invalid API version
                                | Some(CloseCode::Library(4013))  // Invalid intent(s)
                                | Some(CloseCode::Library(4014))  // Disallowed intent(s)
                            );
                            if is_fatal {
                                return Err(format!(
                                    "Discord: fatal close code {:?}: {reason}. \
                                     Check Bot settings at https://discord.com/developers/applications",
                                    code
                                ));
                            }
                            break;
                        }
                        None => {
                            warn!("Discord: WebSocket stream ended (None)");
                            break;
                        }
                        Some(Err(e)) => {
                            warn!(error = %e, "Discord: WebSocket read error");
                            continue;
                        }
                        Some(Ok(other)) => {
                            debug!(msg_type = ?other, "Discord: non-text WebSocket frame");
                            continue;
                        }
                    };

                    let event: serde_json::Value = match serde_json::from_str(msg.as_ref()) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };

                    // Track sequence number from all dispatch events
                    if let Some(s) = event.get("s").and_then(serde_json::Value::as_i64) {
                        sequence = s;
                    }

                    let op = event.get("op").and_then(serde_json::Value::as_u64).unwrap_or(0);

                    match op {
                        // Op 1: Server requests an immediate heartbeat
                        1 => {
                            let d = if sequence >= 0 { json!(sequence) } else { json!(null) };
                            let hb = json!({"op": 1, "d": d});
                            if write.send(WsMessage::Text(hb.to_string().into())).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        // Op 7: Reconnect
                        7 => {
                            warn!("Discord: received Reconnect (op 7), will restart");
                            break;
                        }
                        // Op 9: Invalid Session
                        9 => {
                            warn!("Discord: received Invalid Session (op 9), will restart");
                            break;
                        }
                        _ => {}
                    }

                    // Dispatch events (opcode 0 with "t" field)
                    let event_type = event.get("t").and_then(|t| t.as_str()).unwrap_or("");

                    // Log READY event — confirms connection is fully established
                    if event_type == "READY" {
                        let username = event.get("d")
                            .and_then(|d| d.get("user"))
                            .and_then(|u| u.get("username"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown");
                        let guilds = event.get("d")
                            .and_then(|d| d.get("guilds"))
                            .and_then(|g| g.as_array())
                            .map(|g| g.len())
                            .unwrap_or(0);
                        info!(
                            username = %username,
                            guilds = guilds,
                            bot_user_id = %bot_user_id,
                            "Discord: READY — bot is online and receiving events"
                        );
                        continue;
                    }

                    // Handle INTERACTION_CREATE (slash commands, buttons, modals)
                    if event_type == "INTERACTION_CREATE" {
                        if let Some(d) = event.get("d") {
                            debug!("Discord: INTERACTION_CREATE received");
                            let interaction_data = d.clone();
                            let client = self.client.clone();
                            let app_id = self.application_id.clone();
                            let token = self.bot_token.clone();
                            tokio::spawn(async move {
                                let handler = interactions::InteractionHandler::new(
                                    client, &app_id, &token,
                                );
                                if let Err(e) = handler.handle_event(&interaction_data).await {
                                    warn!(error = %e, "Discord: interaction handling failed");
                                }
                            });
                        }
                        continue;
                    }

                    // Only handle MESSAGE_CREATE for message processing
                    if event_type != "MESSAGE_CREATE" {
                        continue;
                    }

                    let Some(d) = event.get("d") else {
                        continue;
                    };

                    // Parse into typed struct
                    let msg_event: DiscordMessageCreate = match serde_json::from_value(d.clone()) {
                        Ok(m) => m,
                        Err(e) => {
                            warn!(error = %e, "Discord: failed to parse MESSAGE_CREATE — raw payload logged at debug level");
                            debug!(payload = %d, "Discord: unparseable MESSAGE_CREATE payload");
                            continue;
                        }
                    };

                    // Diagnostic: log every inbound message for debugging
                    info!(
                        author = %msg_event.author.username,
                        author_id = %msg_event.author.id,
                        is_bot = msg_event.author.bot.unwrap_or(false),
                        content_len = msg_event.content.len(),
                        content_empty = msg_event.content.is_empty(),
                        channel_id = %msg_event.channel_id,
                        "Discord: MESSAGE_CREATE received"
                    );

                    // If content is empty, the bot likely lacks the MESSAGE_CONTENT
                    // privileged intent. Log a clear diagnostic.
                    if msg_event.content.is_empty() {
                        warn!(
                            author = %msg_event.author.username,
                            "Discord: message content is EMPTY — the MESSAGE_CONTENT \
                             privileged intent may not be enabled in the Discord Developer \
                             Portal (Bot → Privileged Gateway Intents → Message Content Intent)"
                        );
                    }

                    // Normalize (filters bots, allowlist, mention-only, etc.)
                    let Some(normalized) = self.normalize_event(&msg_event, &bot_user_id) else {
                        info!(
                            author = %msg_event.author.username,
                            bot_user_id = %bot_user_id,
                            mention_only = self.mention_only,
                            content_preview = %msg_event.content.chars().take(50).collect::<String>(),
                            "Discord: message filtered out by normalize_event"
                        );
                        continue;
                    };

                    // Process attachments and enrich body
                    let normalized = {
                        let mut m = normalized;
                        let attachments = msg_event
                            .attachments
                            .as_deref()
                            .unwrap_or(&[]);
                        let attachment_text =
                            process_attachments(attachments, &self.client).await;
                        if !attachment_text.is_empty() {
                            m.body_for_agent = Some(format!(
                                "{}\n\n[Attachments]\n{attachment_text}",
                                m.body
                            ));
                        }
                        m
                    };

                    // Fire-and-forget ACK reaction
                    {
                        let ack_token = self.bot_token.clone();
                        let ack_channel_id = msg_event.channel_id.clone();
                        let ack_message_id = msg_event.id.clone();
                        tokio::spawn(async move {
                            let emoji = random_discord_ack_reaction();
                            let encoded = encode_emoji_for_discord(emoji);
                            let url = format!(
                                "{DISCORD_API_BASE}/channels/{ack_channel_id}/messages/{ack_message_id}/reactions/{encoded}/@me"
                            );
                            let _ = reqwest::Client::new()
                                .put(&url)
                                .header("Authorization", format!("Bot {ack_token}"))
                                .header("Content-Length", "0")
                                .send()
                                .await;
                        });
                    }

                    // Auto-discover channel/guild from first inbound message
                    // so default_origin() works for cross-channel sends.
                    if let clawdesk_types::message::MessageOrigin::Discord {
                        guild_id, channel_id, ..
                    } = &normalized.origin {
                        let prev = self.discovered_channel_id.load(Ordering::Relaxed);
                        if prev == 0 {
                            self.discovered_channel_id.store(*channel_id, Ordering::Relaxed);
                            self.discovered_guild_id.store(*guild_id, Ordering::Relaxed);
                            info!(
                                channel_id = channel_id,
                                guild_id = guild_id,
                                "Discord: auto-discovered default channel from first inbound message"
                            );
                        }
                    }

                    // Dispatch to message sink
                    sink.on_message(normalized).await;
                }
            }
        }

        info!("Discord: gateway loop ended");
        Ok(())
    }
}

#[async_trait]
impl Channel for DiscordChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Discord
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Discord".into(),
            supports_threading: true,
            supports_streaming: true,
            supports_reactions: true,
            supports_media: true,
            supports_groups: true,
            max_message_length: Some(DISCORD_MAX_MESSAGE_LENGTH),
        }
    }

    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);

        // Verify bot token
        let resp = self
            .client
            .get(&self.api_url("/users/@me"))
            .header("Authorization", format!("Bot {}", self.bot_token))
            .send()
            .await
            .map_err(|e| format!("Discord auth check failed: {e}"))?;

        if !resp.status().is_success() {
            return Err("Discord bot token is invalid".into());
        }

        let user: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("failed to parse Discord user: {e}"))?;

        info!(
            bot = user
                .get("username")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown"),
            "Discord bot verified"
        );

        // Spawn the Gateway WebSocket event loop on a background task.
        // We create a lightweight clone of this channel's configuration so the
        // spawned future has 'static lifetime (avoids &self borrow issues).
        let gw_channel = DiscordChannel::new(
            self.bot_token.clone(),
            self.application_id.clone(),
            self.allowed_guild_ids.clone(),
            self.allowed_users.clone(),
            self.listen_to_bots,
            self.mention_only,
            self.default_channel_id,
        );
        gw_channel.running.store(true, Ordering::Relaxed);

        // Supervised listener with exponential backoff.
        // Always reconnects unless `running` is set to false via stop().
        // Backoff: 2s → 5s → 10s → 20s → 40s → 80s → 120s cap.
        tokio::spawn(async move {
            const BACKOFF_STEPS: &[u64] = &[2, 5, 10, 20, 40, 80, 120];
            let mut consecutive_failures: usize = 0;

            loop {
                if !gw_channel.running.load(Ordering::Relaxed) {
                    info!("Discord: shutdown requested — exiting supervised listener");
                    break;
                }

                match gw_channel.gateway_loop(Arc::clone(&sink)).await {
                    Ok(()) => {
                        // Normal exit (op 7 Reconnect, op 9 Invalid Session, or
                        // clean WebSocket close). If running is still true, reconnect
                        // immediately — this is expected Discord gateway behavior.
                        if !gw_channel.running.load(Ordering::Relaxed) {
                            info!("Discord: gateway loop ended, shutdown requested");
                            break;
                        }
                        info!("Discord: gateway loop ended normally — reconnecting in 2s");
                        consecutive_failures = 0;
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    Err(e) => {
                        if !gw_channel.running.load(Ordering::Relaxed) {
                            info!("Discord: gateway error during shutdown — exiting");
                            break;
                        }
                        // Fatal errors (auth failure, disallowed intents) — stop retrying
                        let err_str = e.to_string();
                        if err_str.contains("fatal close code") {
                            error!(
                                error = %e,
                                "Discord: FATAL — stopping reconnect. Fix the issue and restart."
                            );
                            gw_channel.running.store(false, Ordering::Relaxed);
                            break;
                        }
                        let delay = BACKOFF_STEPS
                            .get(consecutive_failures)
                            .copied()
                            .unwrap_or(120);
                        warn!(
                            error = %e,
                            attempt = consecutive_failures + 1,
                            backoff_secs = delay,
                            "Discord: gateway error — reconnecting with backoff"
                        );
                        consecutive_failures += 1;
                        tokio::time::sleep(Duration::from_secs(delay)).await;
                    }
                }
            }
        });

        info!("Discord channel started — gateway loop spawned");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let channel_id = match &msg.origin {
            clawdesk_types::message::MessageOrigin::Discord { channel_id, .. } => {
                channel_id.to_string()
            }
            _ => return Err("cannot send Discord message without Discord origin".into()),
        };

        let chunks = split_message_for_discord(&msg.body);

        let mut last_message_id = String::new();

        for (i, chunk) in chunks.iter().enumerate() {
            let mut body = json!({ "content": chunk });

            // Attach reply reference to the first chunk only
            if i == 0 {
                if let Some(reply_to) = &msg.reply_to {
                    body["message_reference"] = json!({ "message_id": reply_to });
                }
            }

            let url = self.api_url(&format!("/channels/{channel_id}/messages"));
            let response = self
                .client
                .post(&url)
                .header("Authorization", format!("Bot {}", self.bot_token))
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("Discord send failed: {e}"))?;

            if !response.status().is_success() {
                let status = response.status();
                let error_body = response.text().await.unwrap_or_default();
                return Err(format!("Discord API error ({status}): {error_body}"));
            }

            let result: serde_json::Value = response
                .json()
                .await
                .map_err(|e| format!("failed to parse send response: {e}"))?;

            last_message_id = result
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Small delay between chunks to avoid rate limiting
            if i < chunks.len() - 1 {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }

        Ok(DeliveryReceipt {
            channel: ChannelId::Discord,
            message_id: last_message_id,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        self.shutdown.notify_waiters();

        // Abort all typing indicator tasks
        if let Ok(mut guard) = self.typing_handles.lock() {
            for (_, handle) in guard.drain() {
                handle.abort();
            }
        }

        info!("Discord channel stopped");
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn default_origin(&self) -> Option<clawdesk_types::message::MessageOrigin> {
        // Priority: explicit default_channel_id config → auto-discovered channel_id
        let channel_id = self.default_channel_id
            .or_else(|| {
                let discovered = self.discovered_channel_id.load(Ordering::Relaxed);
                if discovered != 0 { Some(discovered) } else { None }
            })?;

        let guild_id = self.allowed_guild_ids.first().copied()
            .unwrap_or_else(|| self.discovered_guild_id.load(Ordering::Relaxed));

        info!(
            channel_id = channel_id,
            guild_id = guild_id,
            source = if self.default_channel_id.is_some() { "config" } else { "discovered" },
            "Discord: providing default_origin for cross-channel send"
        );

        Some(clawdesk_types::message::MessageOrigin::Discord {
            guild_id,
            channel_id,
            message_id: 0,
            is_dm: false,
            thread_id: None,
        })
    }
}

#[async_trait]
impl Threaded for DiscordChannel {
    async fn send_to_thread(
        &self,
        thread_id: &str,
        msg: OutboundMessage,
    ) -> Result<DeliveryReceipt, String> {
        // Discord threads are just channels — send to thread_id as channel_id
        let chunks = split_message_for_discord(&msg.body);
        let mut last_message_id = String::new();

        for (i, chunk) in chunks.iter().enumerate() {
            let body = json!({ "content": chunk });
            let url = self.api_url(&format!("/channels/{thread_id}/messages"));

            let response = self
                .client
                .post(&url)
                .header("Authorization", format!("Bot {}", self.bot_token))
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("Discord thread send failed: {e}"))?;

            if !response.status().is_success() {
                let status = response.status();
                let error_body = response.text().await.unwrap_or_default();
                return Err(format!("Discord thread API error ({status}): {error_body}"));
            }

            last_message_id = response
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v.get("id").and_then(|v| v.as_str()).map(String::from))
                .unwrap_or_default();

            if i < chunks.len() - 1 {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }

        Ok(DeliveryReceipt {
            channel: ChannelId::Discord,
            message_id: last_message_id,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn create_thread(
        &self,
        parent_msg_id: &str,
        title: &str,
    ) -> Result<String, String> {
        // Use compound message ID format: "channel_id:message_id"
        let (channel_id, message_id) = parse_compound_msg_id(parent_msg_id);

        let url = self.api_url(&format!(
            "/channels/{channel_id}/messages/{message_id}/threads"
        ));

        let body = json!({
            "name": &title[..title.len().min(100)], // Discord thread names max 100 chars
            "auto_archive_duration": 1440, // 24 hours
        });

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Discord create_thread failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let err = response.text().await.unwrap_or_default();
            return Err(format!("Discord create_thread API error ({status}): {err}"));
        }

        let result: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("failed to parse thread response: {e}"))?;

        let thread_id = result
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or("no thread id in response")?
            .to_string();

        debug!(thread_id = %thread_id, parent = parent_msg_id, title, "Discord thread created");
        Ok(thread_id)
    }
}

#[async_trait]
impl Streaming for DiscordChannel {
    async fn send_streaming(&self, initial: OutboundMessage) -> Result<StreamHandle, String> {
        // Extract channel_id from the origin BEFORE sending, so we can capture
        // it in the streaming update closure.
        let channel_id = match &initial.origin {
            clawdesk_types::message::MessageOrigin::Discord { channel_id, .. } => {
                channel_id.to_string()
            }
            _ => return Err("cannot stream Discord message without Discord origin".into()),
        };

        let receipt = self.send(initial).await?;
        let msg_id = receipt.message_id.clone();

        // Capture bot_token and async client for the edit-in-place closure.
        let bot_token = self.bot_token.clone();
        let client = self.client.clone();

        let edit_url = format!("{DISCORD_API_BASE}/channels/{channel_id}/messages/{msg_id}");

        // Grab a handle to the current tokio runtime so the sync closure can
        // dispatch async work without pulling in reqwest/blocking.
        let handle = tokio::runtime::Handle::current();

        // Discord streaming: edit the initial message in place via PATCH.
        Ok(StreamHandle {
            message_id: msg_id,
            update_fn: Box::new(move |text| {
                // Truncate to Discord's 2000-char limit.
                let content: String = text.chars().take(DISCORD_MAX_MESSAGE_LENGTH).collect();
                let body = serde_json::json!({ "content": content });

                // Run the async PATCH on a dedicated thread to avoid
                // "cannot block inside a runtime" panics.
                let client = client.clone();
                let edit_url = edit_url.clone();
                let bot_token = bot_token.clone();
                let h = handle.clone();

                let join = std::thread::spawn(move || {
                    h.block_on(async {
                        let resp = client
                            .patch(&edit_url)
                            .header("Authorization", format!("Bot {}", bot_token))
                            .json(&body)
                            .send()
                            .await
                            .map_err(|e| format!("Discord edit request failed: {e}"))?;

                        if resp.status().is_success() {
                            Ok(())
                        } else {
                            Err(format!("Discord edit failed: {}", resp.status()))
                        }
                    })
                });

                join.join()
                    .map_err(|_| "Discord edit thread panicked".to_string())?
            }),
        })
    }
}

#[async_trait]
impl Reactions for DiscordChannel {
    async fn add_reaction(&self, msg_id: &str, emoji: &str) -> Result<(), String> {
        // msg_id format: "channel_id:message_id" for compound IDs
        let (channel_id, message_id) = parse_compound_msg_id(msg_id);
        let encoded_emoji = encode_emoji_for_discord(emoji);
        let url = format!(
            "{DISCORD_API_BASE}/channels/{channel_id}/messages/{message_id}/reactions/{encoded_emoji}/@me"
        );

        let resp = self
            .client
            .put(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .header("Content-Length", "0")
            .send()
            .await
            .map_err(|e| format!("Discord add reaction failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Discord add reaction failed ({status}): {err}"));
        }

        Ok(())
    }

    async fn remove_reaction(&self, msg_id: &str, emoji: &str) -> Result<(), String> {
        let (channel_id, message_id) = parse_compound_msg_id(msg_id);
        let encoded_emoji = encode_emoji_for_discord(emoji);
        let url = format!(
            "{DISCORD_API_BASE}/channels/{channel_id}/messages/{message_id}/reactions/{encoded_emoji}/@me"
        );

        let resp = self
            .client
            .delete(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .send()
            .await
            .map_err(|e| format!("Discord remove reaction failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Discord remove reaction failed ({status}): {err}"));
        }

        Ok(())
    }
}

// ─── Helper functions ────────────────────────────────────────────────

/// Parse a compound message ID "channel_id:message_id" for reaction APIs.
/// Falls back to using `msg_id` as both channel and message if no colon found.
fn parse_compound_msg_id(msg_id: &str) -> (&str, &str) {
    if let Some((channel, message)) = msg_id.split_once(':') {
        (channel, message)
    } else {
        (msg_id, msg_id)
    }
}

/// Split a message into chunks that respect Discord's 2000-character limit.
/// Tries to split at word boundaries when possible.
fn split_message_for_discord(message: &str) -> Vec<String> {
    if message.chars().count() <= DISCORD_MAX_MESSAGE_LENGTH {
        return vec![message.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = message;

    while !remaining.is_empty() {
        // Find the byte offset for the 2000th character boundary.
        let hard_split = remaining
            .char_indices()
            .nth(DISCORD_MAX_MESSAGE_LENGTH)
            .map_or(remaining.len(), |(idx, _)| idx);

        let chunk_end = if hard_split == remaining.len() {
            hard_split
        } else {
            // Try to find a good break point (newline, then space)
            let search_area = &remaining[..hard_split];

            // Prefer splitting at newline
            if let Some(pos) = search_area.rfind('\n') {
                if search_area[..pos].chars().count() >= DISCORD_MAX_MESSAGE_LENGTH / 2 {
                    pos + 1
                } else {
                    search_area.rfind(' ').map_or(hard_split, |space| space + 1)
                }
            } else if let Some(pos) = search_area.rfind(' ') {
                pos + 1
            } else {
                // Hard split at the limit
                hard_split
            }
        };

        chunks.push(remaining[..chunk_end].to_string());
        remaining = &remaining[chunk_end..];
    }

    chunks
}

/// URL-encode a Unicode emoji for use in Discord reaction API paths.
///
/// Discord's reaction endpoints accept raw Unicode emoji in the URL path,
/// but they must be percent-encoded per RFC 3986. Custom guild emojis use
/// the `name:id` format and are passed through unencoded.
fn encode_emoji_for_discord(emoji: &str) -> String {
    if emoji.contains(':') {
        return emoji.to_string();
    }
    let mut encoded = String::new();
    for byte in emoji.as_bytes() {
        encoded.push_str(&format!("%{byte:02X}"));
    }
    encoded
}

fn mention_tags(bot_user_id: &str) -> [String; 2] {
    [format!("<@{bot_user_id}>"), format!("<@!{bot_user_id}>")]
}

fn contains_bot_mention(content: &str, bot_user_id: &str) -> bool {
    let tags = mention_tags(bot_user_id);
    content.contains(&tags[0]) || content.contains(&tags[1])
}

/// Normalize incoming Discord message content.
///
/// Returns `None` if the message should be ignored (empty, or doesn't
/// mention the bot when in mention-only mode).
fn normalize_incoming_content(
    content: &str,
    mention_only: bool,
    bot_user_id: &str,
) -> Option<String> {
    if content.is_empty() {
        return None;
    }

    if mention_only && !contains_bot_mention(content, bot_user_id) {
        return None;
    }

    let mut normalized = content.to_string();
    if mention_only {
        for tag in mention_tags(bot_user_id) {
            normalized = normalized.replace(&tag, " ");
        }
    }

    let normalized = normalized.trim().to_string();
    if normalized.is_empty() {
        return None;
    }

    Some(normalized)
}

/// Process Discord message attachments and return a string to append to
/// the agent message context.
///
/// Only `text/*` MIME types are fetched and inlined. All other types are
/// silently skipped. Fetch errors are logged as warnings.
async fn process_attachments(
    attachments: &[serde_json::Value],
    client: &Client,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    for att in attachments {
        let ct = att
            .get("content_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let name = att
            .get("filename")
            .and_then(|v| v.as_str())
            .unwrap_or("file");
        let Some(url) = att.get("url").and_then(|v| v.as_str()) else {
            warn!(name, "discord: attachment has no url, skipping");
            continue;
        };
        if ct.starts_with("text/") {
            match client.get(url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(text) = resp.text().await {
                        parts.push(format!("[{name}]\n{text}"));
                    }
                }
                Ok(resp) => {
                    warn!(name, status = %resp.status(), "discord attachment fetch failed");
                }
                Err(e) => {
                    warn!(name, error = %e, "discord attachment fetch error");
                }
            }
        } else {
            debug!(
                name,
                content_type = ct,
                "discord: skipping unsupported attachment type"
            );
        }
    }
    parts.join("\n---\n")
}

/// Pick a random index from [0, len) using system time as entropy.
fn pick_uniform_index(len: usize) -> usize {
    debug_assert!(len > 0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as usize;
    nanos % len
}

fn random_discord_ack_reaction() -> &'static str {
    DISCORD_ACK_REACTIONS[pick_uniform_index(DISCORD_ACK_REACTIONS.len())]
}

/// Minimal base64 decode — only needs to decode the user ID portion of a
/// Discord bot token.
#[allow(clippy::cast_possible_truncation)]
fn base64_decode(input: &str) -> Option<String> {
    let padded = match input.len() % 4 {
        2 => format!("{input}=="),
        3 => format!("{input}="),
        _ => input.to_string(),
    };

    let mut bytes = Vec::new();
    let chars: Vec<u8> = padded.bytes().collect();

    for chunk in chars.chunks(4) {
        if chunk.len() < 4 {
            break;
        }

        let mut v = [0usize; 4];
        for (i, &b) in chunk.iter().enumerate() {
            if b == b'=' {
                v[i] = 0;
            } else {
                v[i] = BASE64_ALPHABET.iter().position(|&a| a == b)?;
            }
        }

        bytes.push(((v[0] << 2) | (v[1] >> 4)) as u8);
        if chunk[2] != b'=' {
            bytes.push((((v[1] & 0xF) << 4) | (v[2] >> 2)) as u8);
        }
        if chunk[3] != b'=' {
            bytes.push((((v[2] & 0x3) << 6) | v[3]) as u8);
        }
    }

    String::from_utf8(bytes).ok()
}

// ─── Discord API types ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DiscordMessageCreate {
    id: String,
    channel_id: String,
    guild_id: Option<String>,
    author: DiscordUser,
    content: String,
    member: Option<DiscordMember>,
    attachments: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct DiscordUser {
    id: String,
    username: String,
    bot: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct DiscordMember {
    nick: Option<String>,
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_channel() -> DiscordChannel {
        DiscordChannel::new(
            "fake".into(),
            "app_id".into(),
            vec![],
            vec!["*".into()],
            false,
            false,
            None,
        )
    }

    #[test]
    fn channel_id_is_discord() {
        let ch = test_channel();
        assert_eq!(ch.id(), ChannelId::Discord);
    }

    #[test]
    fn meta_max_message_length() {
        let ch = test_channel();
        assert_eq!(ch.meta().max_message_length, Some(2000));
    }

    #[test]
    fn base64_decode_bot_id() {
        let decoded = base64_decode("MTIzNDU2");
        assert_eq!(decoded, Some("123456".to_string()));
    }

    #[test]
    fn bot_user_id_extraction() {
        let token = "MTIzNDU2.fake.hmac";
        let id = DiscordChannel::bot_user_id_from_token(token);
        assert_eq!(id, Some("123456".to_string()));
    }

    #[test]
    fn empty_allowlist_denies_everyone() {
        let ch = DiscordChannel::new(
            "fake".into(),
            "app_id".into(),
            vec![],
            vec![],
            false,
            false,
            None,
        );
        assert!(!ch.is_user_allowed("12345"));
    }

    #[test]
    fn wildcard_allows_everyone() {
        let ch = test_channel();
        assert!(ch.is_user_allowed("12345"));
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn specific_allowlist_filters() {
        let ch = DiscordChannel::new(
            "fake".into(),
            "app_id".into(),
            vec![],
            vec!["111".into(), "222".into()],
            false,
            false,
            None,
        );
        assert!(ch.is_user_allowed("111"));
        assert!(ch.is_user_allowed("222"));
        assert!(!ch.is_user_allowed("333"));
    }

    #[test]
    fn guild_filter_empty_allows_all() {
        let ch = test_channel();
        assert!(ch.is_allowed_guild(12345));
    }

    #[test]
    fn guild_filter_specific() {
        let ch = DiscordChannel::new(
            "fake".into(),
            "app_id".into(),
            vec![100, 200],
            vec!["*".into()],
            false,
            false,
            None,
        );
        assert!(ch.is_allowed_guild(100));
        assert!(ch.is_allowed_guild(200));
        assert!(!ch.is_allowed_guild(300));
    }

    #[test]
    fn contains_bot_mention_supports_both_forms() {
        assert!(contains_bot_mention("hi <@12345>", "12345"));
        assert!(contains_bot_mention("hi <@!12345>", "12345"));
        assert!(!contains_bot_mention("hi <@99999>", "12345"));
    }

    #[test]
    fn normalize_incoming_requires_mention_when_enabled() {
        let result = normalize_incoming_content("hello there", true, "12345");
        assert!(result.is_none());
    }

    #[test]
    fn normalize_incoming_strips_mentions() {
        let result = normalize_incoming_content("  <@!12345> run status  ", true, "12345");
        assert_eq!(result.as_deref(), Some("run status"));
    }

    #[test]
    fn normalize_incoming_rejects_empty_after_strip() {
        let result = normalize_incoming_content("<@12345>", true, "12345");
        assert!(result.is_none());
    }

    // ── Message splitting ────────────────────────────────────────────

    #[test]
    fn split_short_message_under_limit() {
        let msg = "Hello, world!";
        let chunks = split_message_for_discord(msg);
        assert_eq!(chunks, vec![msg]);
    }

    #[test]
    fn split_message_exactly_at_limit() {
        let msg = "a".repeat(DISCORD_MAX_MESSAGE_LENGTH);
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn split_message_just_over_limit() {
        let msg = "a".repeat(DISCORD_MAX_MESSAGE_LENGTH + 1);
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
        assert_eq!(chunks[1].chars().count(), 1);
    }

    #[test]
    fn split_preserves_content() {
        let original = "Hello world! This is a test. ".repeat(200);
        let chunks = split_message_for_discord(&original);
        let reconstructed = chunks.concat();
        assert_eq!(reconstructed, original);
    }

    #[test]
    fn split_unicode_content() {
        let msg = "🦀 Rust! ".repeat(500);
        let chunks = split_message_for_discord(&msg);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= DISCORD_MAX_MESSAGE_LENGTH);
        }
        let reconstructed = chunks.concat();
        assert_eq!(reconstructed, msg);
    }

    #[test]
    fn split_hard_split_no_whitespace() {
        let msg = "a".repeat(5000);
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
        assert_eq!(chunks[1].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
        assert_eq!(chunks[2].chars().count(), 1000);
    }

    // ── Emoji encoding ──────────────────────────────────────────────

    #[test]
    fn encode_emoji_unicode_percent_encodes() {
        let encoded = encode_emoji_for_discord("\u{1F440}");
        assert_eq!(encoded, "%F0%9F%91%80");
    }

    #[test]
    fn encode_emoji_custom_guild_passthrough() {
        let encoded = encode_emoji_for_discord("custom_emoji:123456789");
        assert_eq!(encoded, "custom_emoji:123456789");
    }

    // ── Compound message ID ─────────────────────────────────────────

    #[test]
    fn parse_compound_msg_id_with_colon() {
        let (ch, msg) = parse_compound_msg_id("123456:789012");
        assert_eq!(ch, "123456");
        assert_eq!(msg, "789012");
    }

    #[test]
    fn parse_compound_msg_id_without_colon() {
        let (ch, msg) = parse_compound_msg_id("789012");
        assert_eq!(ch, "789012");
        assert_eq!(msg, "789012");
    }

    // ── Typing handles ──────────────────────────────────────────────

    #[test]
    fn typing_handles_start_empty() {
        let ch = test_channel();
        let guard = ch.typing_handles.lock().unwrap();
        assert!(guard.is_empty());
    }

    #[tokio::test]
    async fn start_typing_sets_handle() {
        let ch = test_channel();
        let _ = ch.start_typing("123456").await;
        let guard = ch.typing_handles.lock().unwrap();
        assert!(guard.contains_key("123456"));
    }

    #[tokio::test]
    async fn stop_typing_clears_handle() {
        let ch = test_channel();
        let _ = ch.start_typing("123456").await;
        let _ = ch.stop_typing("123456").await;
        let guard = ch.typing_handles.lock().unwrap();
        assert!(!guard.contains_key("123456"));
    }

    #[tokio::test]
    async fn stop_typing_is_idempotent() {
        let ch = test_channel();
        assert!(ch.stop_typing("123456").await.is_ok());
        assert!(ch.stop_typing("123456").await.is_ok());
    }

    #[tokio::test]
    async fn process_attachments_empty_returns_empty() {
        let client = Client::new();
        let result = process_attachments(&[], &client).await;
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn process_attachments_skips_unsupported() {
        let client = Client::new();
        let attachments = vec![serde_json::json!({
            "url": "https://example.com/doc.pdf",
            "filename": "doc.pdf",
            "content_type": "application/pdf"
        })];
        let result = process_attachments(&attachments, &client).await;
        assert!(result.is_empty());
    }
}
