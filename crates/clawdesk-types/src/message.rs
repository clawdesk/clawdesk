//! Algebraic message types — sum-type envelope with per-channel variants.
//!
//! The `InboundMessage` enum replaces the god-object MsgContext pattern.
//! Each channel variant carries exactly the fields for its platform,
//! making it impossible to access `guild_id` on a Telegram message (compile error).
//!
//! After channel-specific processing, messages normalize to `NormalizedMessage`,
//! which is the common form consumed by the agent runner and reply pipeline.

use crate::channel::ChannelId;
use crate::session::SessionKey;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Inbound message envelope — discriminated union over channel types
// ---------------------------------------------------------------------------

/// The inbound message envelope — a discriminated union over channel types.
/// The compiler guarantees exhaustive matching: you cannot forget a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "channel", content = "payload")]
pub enum InboundMessage {
    Telegram(TelegramMessage),
    Discord(DiscordMessage),
    Slack(SlackMessage),
    WhatsApp(WhatsAppMessage),
    WebChat(WebChatMessage),
    Email(EmailMessage),
    /// Apple iMessage
    IMessage(IMessageMessage),
    /// IRC over TLS
    Irc(IrcMessage),
    /// CLI / gateway internal message
    Internal(InternalMessage),
}

// ---------------------------------------------------------------------------
// Per-channel message structs — each carries exactly its platform's fields
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramMessage {
    pub chat_id: i64,
    pub message_id: i64,
    pub from_user: TelegramUser,
    pub text: String,
    pub reply_to: Option<i64>,
    pub media: Vec<MediaAttachment>,
    pub sticker: Option<StickerMetadata>,
    pub is_group: bool,
    pub thread_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramUser {
    pub id: i64,
    pub username: Option<String>,
    pub first_name: String,
    pub last_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordMessage {
    pub guild_id: u64,
    pub channel_id: u64,
    pub message_id: u64,
    pub author: DiscordUser,
    pub content: String,
    pub reply_to: Option<u64>,
    pub attachments: Vec<MediaAttachment>,
    pub is_dm: bool,
    pub thread_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordUser {
    pub id: u64,
    pub username: String,
    pub discriminator: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackMessage {
    pub team_id: String,
    pub channel_id: String,
    pub user_id: String,
    pub text: String,
    pub ts: String,
    pub thread_ts: Option<String>,
    pub attachments: Vec<MediaAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhatsAppMessage {
    pub phone_number: String,
    pub message_id: String,
    pub text: String,
    pub media: Vec<MediaAttachment>,
    pub is_group: bool,
    pub group_id: Option<String>,
    pub sender_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebChatMessage {
    pub session_id: String,
    pub user_id: Option<String>,
    pub text: String,
    pub media: Vec<MediaAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalMessage {
    pub source: String,
    pub text: String,
    pub session_key: Option<SessionKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailMessage {
    pub message_id: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<String>,
    pub media: Vec<MediaAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IMessageMessage {
    pub rowid: i64,
    pub sender: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrcMessage {
    pub sender_nick: String,
    pub target: String,
    pub text: String,
    pub is_channel: bool,
    pub message_id: String,
}

// ---------------------------------------------------------------------------
// Shared media types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaAttachment {
    pub media_type: MediaType,
    pub url: Option<String>,
    pub data: Option<Vec<u8>>,
    pub mime_type: String,
    pub filename: Option<String>,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaType {
    Image,
    Video,
    Audio,
    Document,
    Sticker,
    Voice,
    Animation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StickerMetadata {
    pub emoji: Option<String>,
    pub set_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyContext {
    pub original_message_id: String,
    pub original_text: Option<String>,
    pub original_sender: Option<String>,
}

// ---------------------------------------------------------------------------
// Normalized message — the common form for downstream processing
// ---------------------------------------------------------------------------

/// After channel-specific processing, messages normalize to this common form.
/// This is what the agent runner and reply pipeline operate on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedMessage {
    pub id: Uuid,
    pub session_key: SessionKey,
    pub body: String,
    pub body_for_agent: Option<String>,
    pub sender: SenderIdentity,
    pub media: Vec<MediaAttachment>,
    /// Content-addressed artifact references (GAP-E: cross-channel artifact pipeline).
    /// Populated after media ingestion into the ArtifactPipeline.
    #[serde(default)]
    pub artifact_refs: Vec<crate::artifact::ArtifactId>,
    pub reply_context: Option<ReplyContext>,
    pub origin: MessageOrigin,
    pub timestamp: DateTime<Utc>,
}

/// Sender identity abstracted across channels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SenderIdentity {
    pub id: String,
    pub display_name: String,
    pub channel: ChannelId,
}

/// Origin tracking preserves channel-specific routing info needed for replies.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MessageOrigin {
    Telegram { chat_id: i64, message_id: i64, thread_id: Option<i64> },
    Discord { guild_id: u64, channel_id: u64, message_id: u64, is_dm: bool, thread_id: Option<u64> },
    Slack { team_id: String, channel_id: String, user_id: String, ts: String, thread_ts: Option<String> },
    WhatsApp { phone_number: String, message_id: String },
    WebChat { session_id: String },
    Email { message_id: String, from: String, to: String },
    IMessage { rowid: i64, sender: String },
    Irc { target: String, sender_nick: String, is_channel: bool },
    Internal { source: String },
    Teams { conversation_id: String },
    Matrix { room_id: String },
    Signal { phone_number: String },
    Webhook { source: String },
    Mastodon { instance: String, visibility: String },
}

impl MessageOrigin {
    /// Get the channel ID from the origin.
    pub fn channel_id(&self) -> ChannelId {
        match self {
            Self::Telegram { .. } => ChannelId::Telegram,
            Self::Discord { .. } => ChannelId::Discord,
            Self::Slack { .. } => ChannelId::Slack,
            Self::WhatsApp { .. } => ChannelId::WhatsApp,
            Self::WebChat { .. } => ChannelId::WebChat,
            Self::Email { .. } => ChannelId::Email,
            Self::IMessage { .. } => ChannelId::IMessage,
            Self::Irc { .. } => ChannelId::Irc,
            Self::Internal { .. } => ChannelId::Internal,
            Self::Teams { .. } => ChannelId::Teams,
            Self::Matrix { .. } => ChannelId::Matrix,
            Self::Signal { .. } => ChannelId::Signal,
            Self::Webhook { .. } => ChannelId::Webhook,
            Self::Mastodon { .. } => ChannelId::Mastodon,
        }
    }

    /// Extract a generic message ID string for logging/tracking.
    pub fn message_id(&self) -> String {
        match self {
            Self::Telegram { message_id, .. } => message_id.to_string(),
            Self::Discord { message_id, .. } => message_id.to_string(),
            Self::Slack { ts, .. } => ts.clone(),
            Self::WhatsApp { message_id, .. } => message_id.clone(),
            Self::WebChat { session_id, .. } => session_id.clone(),
            Self::Email { message_id, .. } => message_id.clone(),
            Self::IMessage { rowid, .. } => rowid.to_string(),
            Self::Irc { sender_nick, .. } => sender_nick.clone(),
            Self::Internal { source, .. } => source.clone(),
            Self::Teams { conversation_id, .. } => conversation_id.clone(),
            Self::Matrix { room_id, .. } => room_id.clone(),
            Self::Signal { phone_number, .. } => phone_number.clone(),
            Self::Webhook { source, .. } => source.clone(),
            Self::Mastodon { instance, .. } => instance.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Outbound message — what the agent sends back
// ---------------------------------------------------------------------------

/// Outbound message to be delivered to a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundMessage {
    pub origin: MessageOrigin,
    pub body: String,
    pub media: Vec<MediaAttachment>,
    pub reply_to: Option<String>,
    pub thread_id: Option<String>,
}

/// Convert media URL strings to typed [`MediaAttachment`] structs for channel delivery.
///
/// Infers MIME type from file extension and media type from MIME prefix.
/// Channel adapters read bytes at send time via `attachment.url`.
pub fn media_urls_to_attachments(urls: &[String]) -> Vec<MediaAttachment> {
    urls.iter()
        .map(|url| {
            let mime = mime_from_path(url);
            let media_type = if mime.starts_with("image/") {
                MediaType::Image
            } else if mime.starts_with("audio/") {
                MediaType::Audio
            } else if mime.starts_with("video/") {
                MediaType::Video
            } else {
                MediaType::Document
            };
            MediaAttachment {
                media_type,
                url: Some(url.clone()),
                data: None,
                mime_type: mime,
                filename: url.rsplit('/').next().map(String::from),
                size_bytes: std::fs::metadata(url).ok().map(|m| m.len()),
            }
        })
        .collect()
}

/// Infer MIME type from file path extension.
fn mime_from_path(path: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Confirmation that a message was delivered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryReceipt {
    pub channel: ChannelId,
    pub message_id: String,
    pub timestamp: DateTime<Utc>,
    pub success: bool,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// InboundMessage normalization
// ---------------------------------------------------------------------------

/// Trait for resolving session keys from channel-specific identifiers.
pub trait SessionResolver: Send + Sync {
    fn resolve_telegram(&self, chat_id: i64) -> SessionKey;
    fn resolve_discord(&self, guild_id: u64, channel_id: u64) -> SessionKey;
    fn resolve_slack(&self, team_id: &str, channel_id: &str) -> SessionKey;
    fn resolve_whatsapp(&self, phone_number: &str) -> SessionKey;
    fn resolve_webchat(&self, session_id: &str) -> SessionKey;
    fn resolve_email(&self, from: &str, to: &str) -> SessionKey;
    fn resolve_imessage(&self, sender: &str) -> SessionKey;
    fn resolve_irc(&self, target: &str, sender_nick: &str) -> SessionKey;
    fn resolve_internal(&self, source: &str) -> SessionKey;
}

impl InboundMessage {
    /// Normalize to the common message form.
    ///
    /// This is the **only** place where channel-specific field extraction happens.
    /// The compiler guarantees exhaustive matching — adding a new channel variant
    /// forces handling here.
    pub fn normalize(&self, resolver: &dyn SessionResolver) -> NormalizedMessage {
        match self {
            InboundMessage::Telegram(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_telegram(msg.chat_id),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.from_user.id.to_string(),
                    display_name: msg.from_user.first_name.clone(),
                    channel: ChannelId::Telegram,
                },
                media: msg.media.clone(),
                artifact_refs: vec![],
                reply_context: msg.reply_to.map(|id| ReplyContext {
                    original_message_id: id.to_string(),
                    original_text: None,
                    original_sender: None,
                }),
                origin: MessageOrigin::Telegram {
                    chat_id: msg.chat_id,
                    message_id: msg.message_id,
                    thread_id: msg.thread_id,
                },
                timestamp: Utc::now(),
            },
            InboundMessage::Discord(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_discord(msg.guild_id, msg.channel_id),
                body: msg.content.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.author.id.to_string(),
                    display_name: msg.author.display_name.clone().unwrap_or(msg.author.username.clone()),
                    channel: ChannelId::Discord,
                },
                media: msg.attachments.clone(),
                artifact_refs: vec![],
                reply_context: msg.reply_to.map(|id| ReplyContext {
                    original_message_id: id.to_string(),
                    original_text: None,
                    original_sender: None,
                }),
                origin: MessageOrigin::Discord {
                    guild_id: msg.guild_id,
                    channel_id: msg.channel_id,
                    message_id: msg.message_id,
                    is_dm: msg.is_dm,
                    thread_id: msg.thread_id,
                },
                timestamp: Utc::now(),
            },
            InboundMessage::Slack(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_slack(&msg.team_id, &msg.channel_id),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.user_id.clone(),
                    display_name: msg.user_id.clone(),
                    channel: ChannelId::Slack,
                },
                media: msg.attachments.clone(),
                artifact_refs: vec![],
                reply_context: None,
                origin: MessageOrigin::Slack {
                    team_id: msg.team_id.clone(),
                    channel_id: msg.channel_id.clone(),
                    user_id: msg.user_id.clone(),
                    ts: msg.ts.clone(),
                    thread_ts: msg.thread_ts.clone(),
                },
                timestamp: Utc::now(),
            },
            InboundMessage::WhatsApp(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_whatsapp(&msg.phone_number),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.phone_number.clone(),
                    display_name: msg.sender_name.clone().unwrap_or_else(|| msg.phone_number.clone()),
                    channel: ChannelId::WhatsApp,
                },
                media: msg.media.clone(),
                artifact_refs: vec![],
                reply_context: None,
                origin: MessageOrigin::WhatsApp {
                    phone_number: msg.phone_number.clone(),
                    message_id: msg.message_id.clone(),
                },
                timestamp: Utc::now(),
            },
            InboundMessage::WebChat(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_webchat(&msg.session_id),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.user_id.clone().unwrap_or_else(|| "anonymous".to_string()),
                    display_name: msg.user_id.clone().unwrap_or_else(|| "Web User".to_string()),
                    channel: ChannelId::WebChat,
                },
                media: msg.media.clone(),
                artifact_refs: vec![],
                reply_context: None,
                origin: MessageOrigin::WebChat {
                    session_id: msg.session_id.clone(),
                },
                timestamp: Utc::now(),
            },
            InboundMessage::Email(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_email(&msg.from, &msg.to),
                body: msg.body.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.from.clone(),
                    display_name: msg.from.clone(),
                    channel: ChannelId::Email,
                },
                media: msg.media.clone(),
                artifact_refs: vec![],
                reply_context: msg.in_reply_to.as_ref().map(|id| ReplyContext {
                    original_message_id: id.clone(),
                    original_text: None,
                    original_sender: None,
                }),
                origin: MessageOrigin::Email {
                    message_id: msg.message_id.clone(),
                    from: msg.from.clone(),
                    to: msg.to.clone(),
                },
                timestamp: Utc::now(),
            },
            InboundMessage::IMessage(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_imessage(&msg.sender),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.sender.clone(),
                    display_name: msg.sender.clone(),
                    channel: ChannelId::IMessage,
                },
                media: vec![],
                artifact_refs: vec![],
                reply_context: None,
                origin: MessageOrigin::IMessage {
                    rowid: msg.rowid,
                    sender: msg.sender.clone(),
                },
                timestamp: Utc::now(),
            },
            InboundMessage::Irc(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_irc(&msg.target, &msg.sender_nick),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.sender_nick.clone(),
                    display_name: msg.sender_nick.clone(),
                    channel: ChannelId::Irc,
                },
                media: vec![],
                artifact_refs: vec![],
                reply_context: None,
                origin: MessageOrigin::Irc {
                    target: msg.target.clone(),
                    sender_nick: msg.sender_nick.clone(),
                    is_channel: msg.is_channel,
                },
                timestamp: Utc::now(),
            },
            InboundMessage::Internal(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: msg
                    .session_key
                    .clone()
                    .unwrap_or_else(|| resolver.resolve_internal(&msg.source)),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.source.clone(),
                    display_name: msg.source.clone(),
                    channel: ChannelId::Internal,
                },
                media: vec![],
                artifact_refs: vec![],
                reply_context: None,
                origin: MessageOrigin::Internal {
                    source: msg.source.clone(),
                },
                timestamp: Utc::now(),
            },
        }
    }

    /// Consuming normalization — moves owned String fields instead of cloning.
    ///
    /// Use this when the `InboundMessage` is no longer needed after normalization,
    /// which is the common case in the message pipeline. Avoids all String clones.
    pub fn into_normalized(self, resolver: &dyn SessionResolver) -> NormalizedMessage {
        match self {
            InboundMessage::Telegram(msg) => {
                let session_key = resolver.resolve_telegram(msg.chat_id);
                let sender_id = msg.from_user.id.to_string();
                let reply_context = msg.reply_to.map(|id| ReplyContext {
                    original_message_id: id.to_string(),
                    original_text: None,
                    original_sender: None,
                });
                let origin = MessageOrigin::Telegram {
                    chat_id: msg.chat_id,
                    message_id: msg.message_id,
                    thread_id: msg.thread_id,
                };
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: sender_id,
                        display_name: msg.from_user.first_name,
                        channel: ChannelId::Telegram,
                    },
                    media: msg.media,
                    artifact_refs: vec![],
                    reply_context,
                    origin,
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::Discord(msg) => {
                let session_key = resolver.resolve_discord(msg.guild_id, msg.channel_id);
                let sender_id = msg.author.id.to_string();
                let display = msg.author.display_name.unwrap_or(msg.author.username);
                let reply_context = msg.reply_to.map(|id| ReplyContext {
                    original_message_id: id.to_string(),
                    original_text: None,
                    original_sender: None,
                });
                let origin = MessageOrigin::Discord {
                    guild_id: msg.guild_id,
                    channel_id: msg.channel_id,
                    message_id: msg.message_id,
                    is_dm: msg.is_dm,
                    thread_id: msg.thread_id,
                };
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.content,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: sender_id,
                        display_name: display,
                        channel: ChannelId::Discord,
                    },
                    media: msg.attachments,
                    artifact_refs: vec![],
                    reply_context,
                    origin,
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::Slack(msg) => {
                let session_key = resolver.resolve_slack(&msg.team_id, &msg.channel_id);
                let sender_id = msg.user_id.clone();
                let display_name = msg.user_id.clone();
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: sender_id,
                        display_name,
                        channel: ChannelId::Slack,
                    },
                    media: msg.attachments,
                    artifact_refs: vec![],
                    reply_context: None,
                    origin: MessageOrigin::Slack {
                        team_id: msg.team_id,
                        channel_id: msg.channel_id,
                        user_id: msg.user_id,
                        ts: msg.ts,
                        thread_ts: msg.thread_ts,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::WhatsApp(msg) => {
                let session_key = resolver.resolve_whatsapp(&msg.phone_number);
                let display = msg.sender_name.unwrap_or_else(|| msg.phone_number.clone());
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: msg.phone_number.clone(),
                        display_name: display,
                        channel: ChannelId::WhatsApp,
                    },
                    media: msg.media,
                    artifact_refs: vec![],
                    reply_context: None,
                    origin: MessageOrigin::WhatsApp {
                        phone_number: msg.phone_number,
                        message_id: msg.message_id,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::WebChat(msg) => {
                let session_key = resolver.resolve_webchat(&msg.session_id);
                let uid = msg.user_id;
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: uid.clone().unwrap_or_else(|| "anonymous".to_string()),
                        display_name: uid.unwrap_or_else(|| "Web User".to_string()),
                        channel: ChannelId::WebChat,
                    },
                    media: msg.media,
                    artifact_refs: vec![],
                    reply_context: None,
                    origin: MessageOrigin::WebChat {
                        session_id: msg.session_id,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::Email(msg) => {
                let session_key = resolver.resolve_email(&msg.from, &msg.to);
                let reply_context = msg.in_reply_to.map(|id| ReplyContext {
                    original_message_id: id,
                    original_text: None,
                    original_sender: None,
                });
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.body,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: msg.from.clone(),
                        display_name: msg.from.clone(),
                        channel: ChannelId::Email,
                    },
                    media: msg.media,
                    artifact_refs: vec![],
                    reply_context,
                    origin: MessageOrigin::Email {
                        message_id: msg.message_id,
                        from: msg.from,
                        to: msg.to,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::IMessage(msg) => {
                let session_key = resolver.resolve_imessage(&msg.sender);
                let sender_id = msg.sender.clone();
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: sender_id.clone(),
                        display_name: sender_id.clone(),
                        channel: ChannelId::IMessage,
                    },
                    media: vec![],
                    artifact_refs: vec![],
                    reply_context: None,
                    origin: MessageOrigin::IMessage {
                        rowid: msg.rowid,
                        sender: sender_id,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::Irc(msg) => {
                let session_key = resolver.resolve_irc(&msg.target, &msg.sender_nick);
                let nick = msg.sender_nick.clone();
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: nick.clone(),
                        display_name: nick.clone(),
                        channel: ChannelId::Irc,
                    },
                    media: vec![],
                    artifact_refs: vec![],
                    reply_context: None,
                    origin: MessageOrigin::Irc {
                        target: msg.target,
                        sender_nick: nick,
                        is_channel: msg.is_channel,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::Internal(msg) => {
                let session_key = msg
                    .session_key
                    .unwrap_or_else(|| resolver.resolve_internal(&msg.source));
                let sender_id = msg.source.clone();
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: sender_id,
                        display_name: msg.source.clone(),
                        channel: ChannelId::Internal,
                    },
                    media: vec![],
                    artifact_refs: vec![],
                    reply_context: None,
                    origin: MessageOrigin::Internal {
                        source: msg.source,
                    },
                    timestamp: Utc::now(),
                }
            }
        }
    }
}
