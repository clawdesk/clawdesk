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
    Signal(SignalMessage),
    IMessage(IMessageMessage),
    WebChat(WebChatMessage),
    Matrix(MatrixMessage),
    Line(LineMessage),
    GoogleChat(GoogleChatMessage),
    MsTeams(MsTeamsMessage),
    Nostr(NostrMessage),
    Irc(IrcMessage),
    Mattermost(MattermostMessage),
    Email(EmailMessage),
    Feishu(FeishuMessage),
    Twitch(TwitchMessage),
    NextcloudTalk(NextcloudTalkMessage),
    Zalo(ZaloMessage),
    Tlon(TlonMessage),
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
pub struct SignalMessage {
    pub phone_number: String,
    pub message_id: String,
    pub text: String,
    pub media: Vec<MediaAttachment>,
    pub is_group: bool,
    pub group_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IMessageMessage {
    pub apple_id: String,
    pub message_id: String,
    pub text: String,
    pub media: Vec<MediaAttachment>,
    pub is_group: bool,
    pub group_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebChatMessage {
    pub session_id: String,
    pub user_id: Option<String>,
    pub text: String,
    pub media: Vec<MediaAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixMessage {
    pub room_id: String,
    pub event_id: String,
    pub sender: String,
    pub text: String,
    pub media: Vec<MediaAttachment>,
    pub reply_to: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineMessage {
    pub user_id: String,
    pub message_id: String,
    pub text: String,
    pub media: Vec<MediaAttachment>,
    pub reply_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleChatMessage {
    pub space_name: String,
    pub message_name: String,
    pub sender_name: String,
    pub text: String,
    pub thread_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MsTeamsMessage {
    pub tenant_id: String,
    pub team_id: Option<String>,
    pub channel_id: String,
    pub message_id: String,
    pub from_user: String,
    pub text: String,
    pub reply_to: Option<String>,
    pub attachments: Vec<MediaAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NostrMessage {
    pub pubkey: String,
    pub event_id: String,
    pub text: String,
    pub kind: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalMessage {
    pub source: String,
    pub text: String,
    pub session_key: Option<SessionKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrcMessage {
    pub server: String,
    pub channel: String,
    pub nick: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MattermostMessage {
    pub server_url: String,
    pub channel_id: String,
    pub post_id: String,
    pub user_id: String,
    pub username: String,
    pub text: String,
    pub reply_to: Option<String>,
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
pub struct FeishuMessage {
    pub message_id: String,
    pub chat_id: String,
    pub sender_id: String,
    pub sender_name: String,
    pub text: String,
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwitchMessage {
    pub channel_name: String,
    pub user_id: String,
    pub username: String,
    pub display_name: String,
    pub text: String,
    pub message_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextcloudTalkMessage {
    pub base_url: String,
    pub room_token: String,
    pub message_id: i64,
    pub actor_id: String,
    pub actor_display_name: String,
    pub text: String,
    pub parent_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZaloMessage {
    pub user_id: String,
    pub message_id: String,
    pub text: String,
    pub media: Vec<MediaAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlonMessage {
    pub ship: String,
    pub channel_name: String,
    pub author: String,
    pub text: String,
    pub time_sent: i64,
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
    Signal { phone_number: String, message_id: String },
    IMessage { apple_id: String, message_id: String },
    WebChat { session_id: String },
    Matrix { room_id: String, event_id: String },
    Line { user_id: String, reply_token: String },
    GoogleChat { space_name: String, message_name: String },
    MsTeams { tenant_id: String, channel_id: String, message_id: String },
    Nostr { pubkey: String, event_id: String },
    Irc { server: String, channel: String, nick: String },
    Mattermost { server_url: String, channel_id: String, post_id: String },
    Email { message_id: String, from: String, to: String },
    Feishu { chat_id: String, message_id: String },
    Twitch { channel_name: String, message_id: String },
    NextcloudTalk { base_url: String, room_token: String, message_id: i64 },
    Zalo { user_id: String, message_id: String },
    ZaloUser { user_id: String, message_id: String },
    Tlon { ship: String, channel_name: String },
    BlueBubbles { chat_guid: String, message_guid: String, handle: String },
    Internal { source: String },
}

impl MessageOrigin {
    /// Get the channel ID from the origin.
    pub fn channel_id(&self) -> ChannelId {
        match self {
            Self::Telegram { .. } => ChannelId::Telegram,
            Self::Discord { .. } => ChannelId::Discord,
            Self::Slack { .. } => ChannelId::Slack,
            Self::WhatsApp { .. } => ChannelId::WhatsApp,
            Self::Signal { .. } => ChannelId::Signal,
            Self::IMessage { .. } => ChannelId::IMessage,
            Self::WebChat { .. } => ChannelId::WebChat,
            Self::Matrix { .. } => ChannelId::Matrix,
            Self::Line { .. } => ChannelId::Line,
            Self::GoogleChat { .. } => ChannelId::GoogleChat,
            Self::MsTeams { .. } => ChannelId::MsTeams,
            Self::Nostr { .. } => ChannelId::Nostr,
            Self::Irc { .. } => ChannelId::Irc,
            Self::Mattermost { .. } => ChannelId::Mattermost,
            Self::Email { .. } => ChannelId::Email,
            Self::Feishu { .. } => ChannelId::Feishu,
            Self::Twitch { .. } => ChannelId::Twitch,
            Self::NextcloudTalk { .. } => ChannelId::NextcloudTalk,
            Self::Zalo { .. } => ChannelId::Zalo,
            Self::ZaloUser { .. } => ChannelId::ZaloUser,
            Self::Tlon { .. } => ChannelId::Tlon,
            Self::BlueBubbles { .. } => ChannelId::BlueBubbles,
            Self::Internal { .. } => ChannelId::Internal,
        }
    }

    /// Extract a generic message ID string for logging/tracking.
    pub fn message_id(&self) -> String {
        match self {
            Self::Telegram { message_id, .. } => message_id.to_string(),
            Self::Discord { message_id, .. } => message_id.to_string(),
            Self::Slack { ts, .. } => ts.clone(),
            Self::WhatsApp { message_id, .. } => message_id.clone(),
            Self::Signal { message_id, .. } => message_id.clone(),
            Self::IMessage { message_id, .. } => message_id.clone(),
            Self::WebChat { session_id, .. } => session_id.clone(),
            Self::Matrix { event_id, .. } => event_id.clone(),
            Self::Line { reply_token, .. } => reply_token.clone(),
            Self::GoogleChat { message_name, .. } => message_name.clone(),
            Self::MsTeams { message_id, .. } => message_id.clone(),
            Self::Nostr { event_id, .. } => event_id.clone(),
            Self::Irc { nick, .. } => nick.clone(),
            Self::Mattermost { post_id, .. } => post_id.clone(),
            Self::Email { message_id, .. } => message_id.clone(),
            Self::Feishu { message_id, .. } => message_id.clone(),
            Self::Twitch { message_id, .. } => message_id.clone(),
            Self::NextcloudTalk { message_id, .. } => message_id.to_string(),
            Self::Zalo { message_id, .. } => message_id.clone(),
            Self::ZaloUser { message_id, .. } => message_id.clone(),
            Self::Tlon { channel_name, .. } => channel_name.clone(),
            Self::BlueBubbles { message_guid, .. } => message_guid.clone(),
            Self::Internal { source, .. } => source.clone(),
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
    fn resolve_signal(&self, phone_number: &str) -> SessionKey;
    fn resolve_imessage(&self, apple_id: &str) -> SessionKey;
    fn resolve_webchat(&self, session_id: &str) -> SessionKey;
    fn resolve_matrix(&self, room_id: &str) -> SessionKey;
    fn resolve_line(&self, user_id: &str) -> SessionKey;
    fn resolve_googlechat(&self, space_name: &str) -> SessionKey;
    fn resolve_msteams(&self, tenant_id: &str, channel_id: &str) -> SessionKey;
    fn resolve_nostr(&self, pubkey: &str) -> SessionKey;
    fn resolve_irc(&self, server: &str, channel: &str) -> SessionKey;
    fn resolve_mattermost(&self, server_url: &str, channel_id: &str) -> SessionKey;
    fn resolve_email(&self, from: &str, to: &str) -> SessionKey;
    fn resolve_feishu(&self, chat_id: &str) -> SessionKey;
    fn resolve_twitch(&self, channel_name: &str) -> SessionKey;
    fn resolve_nextcloud_talk(&self, base_url: &str, room_token: &str) -> SessionKey;
    fn resolve_zalo(&self, user_id: &str) -> SessionKey;
    fn resolve_tlon(&self, ship: &str, channel_name: &str) -> SessionKey;
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
                reply_context: None,
                origin: MessageOrigin::WhatsApp {
                    phone_number: msg.phone_number.clone(),
                    message_id: msg.message_id.clone(),
                },
                timestamp: Utc::now(),
            },
            InboundMessage::Signal(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_signal(&msg.phone_number),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.phone_number.clone(),
                    display_name: msg.phone_number.clone(),
                    channel: ChannelId::Signal,
                },
                media: msg.media.clone(),
                reply_context: None,
                origin: MessageOrigin::Signal {
                    phone_number: msg.phone_number.clone(),
                    message_id: msg.message_id.clone(),
                },
                timestamp: Utc::now(),
            },
            InboundMessage::IMessage(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_imessage(&msg.apple_id),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.apple_id.clone(),
                    display_name: msg.apple_id.clone(),
                    channel: ChannelId::IMessage,
                },
                media: msg.media.clone(),
                reply_context: None,
                origin: MessageOrigin::IMessage {
                    apple_id: msg.apple_id.clone(),
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
                reply_context: None,
                origin: MessageOrigin::WebChat {
                    session_id: msg.session_id.clone(),
                },
                timestamp: Utc::now(),
            },
            InboundMessage::Matrix(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_matrix(&msg.room_id),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.sender.clone(),
                    display_name: msg.sender.clone(),
                    channel: ChannelId::Matrix,
                },
                media: msg.media.clone(),
                reply_context: msg.reply_to.as_ref().map(|id| ReplyContext {
                    original_message_id: id.clone(),
                    original_text: None,
                    original_sender: None,
                }),
                origin: MessageOrigin::Matrix {
                    room_id: msg.room_id.clone(),
                    event_id: msg.event_id.clone(),
                },
                timestamp: Utc::now(),
            },
            InboundMessage::Line(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_line(&msg.user_id),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.user_id.clone(),
                    display_name: msg.user_id.clone(),
                    channel: ChannelId::Line,
                },
                media: msg.media.clone(),
                reply_context: None,
                origin: MessageOrigin::Line {
                    user_id: msg.user_id.clone(),
                    reply_token: msg.reply_token.clone(),
                },
                timestamp: Utc::now(),
            },
            InboundMessage::GoogleChat(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_googlechat(&msg.space_name),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.sender_name.clone(),
                    display_name: msg.sender_name.clone(),
                    channel: ChannelId::GoogleChat,
                },
                media: vec![],
                reply_context: None,
                origin: MessageOrigin::GoogleChat {
                    space_name: msg.space_name.clone(),
                    message_name: msg.message_name.clone(),
                },
                timestamp: Utc::now(),
            },
            InboundMessage::MsTeams(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_msteams(&msg.tenant_id, &msg.channel_id),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.from_user.clone(),
                    display_name: msg.from_user.clone(),
                    channel: ChannelId::MsTeams,
                },
                media: msg.attachments.clone(),
                reply_context: msg.reply_to.as_ref().map(|id| ReplyContext {
                    original_message_id: id.clone(),
                    original_text: None,
                    original_sender: None,
                }),
                origin: MessageOrigin::MsTeams {
                    tenant_id: msg.tenant_id.clone(),
                    channel_id: msg.channel_id.clone(),
                    message_id: msg.message_id.clone(),
                },
                timestamp: Utc::now(),
            },
            InboundMessage::Nostr(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_nostr(&msg.pubkey),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.pubkey.clone(),
                    display_name: msg.pubkey[..8].to_string(),
                    channel: ChannelId::Nostr,
                },
                media: vec![],
                reply_context: None,
                origin: MessageOrigin::Nostr {
                    pubkey: msg.pubkey.clone(),
                    event_id: msg.event_id.clone(),
                },
                timestamp: Utc::now(),
            },
            InboundMessage::Irc(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_irc(&msg.server, &msg.channel),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.nick.clone(),
                    display_name: msg.nick.clone(),
                    channel: ChannelId::Irc,
                },
                media: vec![],
                reply_context: None,
                origin: MessageOrigin::Irc {
                    server: msg.server.clone(),
                    channel: msg.channel.clone(),
                    nick: msg.nick.clone(),
                },
                timestamp: Utc::now(),
            },
            InboundMessage::Mattermost(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_mattermost(&msg.server_url, &msg.channel_id),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.user_id.clone(),
                    display_name: msg.username.clone(),
                    channel: ChannelId::Mattermost,
                },
                media: vec![],
                reply_context: msg.reply_to.as_ref().map(|id| ReplyContext {
                    original_message_id: id.clone(),
                    original_text: None,
                    original_sender: None,
                }),
                origin: MessageOrigin::Mattermost {
                    server_url: msg.server_url.clone(),
                    channel_id: msg.channel_id.clone(),
                    post_id: msg.post_id.clone(),
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
            InboundMessage::Feishu(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_feishu(&msg.chat_id),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.sender_id.clone(),
                    display_name: msg.sender_name.clone(),
                    channel: ChannelId::Feishu,
                },
                media: vec![],
                reply_context: msg.parent_id.as_ref().map(|id| ReplyContext {
                    original_message_id: id.clone(),
                    original_text: None,
                    original_sender: None,
                }),
                origin: MessageOrigin::Feishu {
                    chat_id: msg.chat_id.clone(),
                    message_id: msg.message_id.clone(),
                },
                timestamp: Utc::now(),
            },
            InboundMessage::Twitch(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_twitch(&msg.channel_name),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.user_id.clone(),
                    display_name: msg.display_name.clone(),
                    channel: ChannelId::Twitch,
                },
                media: vec![],
                reply_context: None,
                origin: MessageOrigin::Twitch {
                    channel_name: msg.channel_name.clone(),
                    message_id: msg.message_id.clone(),
                },
                timestamp: Utc::now(),
            },
            InboundMessage::NextcloudTalk(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_nextcloud_talk(&msg.base_url, &msg.room_token),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.actor_id.clone(),
                    display_name: msg.actor_display_name.clone(),
                    channel: ChannelId::NextcloudTalk,
                },
                media: vec![],
                reply_context: msg.parent_id.map(|id| ReplyContext {
                    original_message_id: id.to_string(),
                    original_text: None,
                    original_sender: None,
                }),
                origin: MessageOrigin::NextcloudTalk {
                    base_url: msg.base_url.clone(),
                    room_token: msg.room_token.clone(),
                    message_id: msg.message_id,
                },
                timestamp: Utc::now(),
            },
            InboundMessage::Zalo(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_zalo(&msg.user_id),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.user_id.clone(),
                    display_name: msg.user_id.clone(),
                    channel: ChannelId::Zalo,
                },
                media: msg.media.clone(),
                reply_context: None,
                origin: MessageOrigin::Zalo {
                    user_id: msg.user_id.clone(),
                    message_id: msg.message_id.clone(),
                },
                timestamp: Utc::now(),
            },
            InboundMessage::Tlon(msg) => NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: resolver.resolve_tlon(&msg.ship, &msg.channel_name),
                body: msg.text.clone(),
                body_for_agent: None,
                sender: SenderIdentity {
                    id: msg.author.clone(),
                    display_name: msg.author.clone(),
                    channel: ChannelId::Tlon,
                },
                media: vec![],
                reply_context: None,
                origin: MessageOrigin::Tlon {
                    ship: msg.ship.clone(),
                    channel_name: msg.channel_name.clone(),
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
                    reply_context: None,
                    origin: MessageOrigin::WhatsApp {
                        phone_number: msg.phone_number,
                        message_id: msg.message_id,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::Signal(msg) => {
                let session_key = resolver.resolve_signal(&msg.phone_number);
                let sender_id = msg.phone_number.clone();
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: sender_id,
                        display_name: msg.phone_number.clone(),
                        channel: ChannelId::Signal,
                    },
                    media: msg.media,
                    reply_context: None,
                    origin: MessageOrigin::Signal {
                        phone_number: msg.phone_number,
                        message_id: msg.message_id,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::IMessage(msg) => {
                let session_key = resolver.resolve_imessage(&msg.apple_id);
                let sender_id = msg.apple_id.clone();
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: sender_id,
                        display_name: msg.apple_id.clone(),
                        channel: ChannelId::IMessage,
                    },
                    media: msg.media,
                    reply_context: None,
                    origin: MessageOrigin::IMessage {
                        apple_id: msg.apple_id,
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
                    reply_context: None,
                    origin: MessageOrigin::WebChat {
                        session_id: msg.session_id,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::Matrix(msg) => {
                let session_key = resolver.resolve_matrix(&msg.room_id);
                let sender_id = msg.sender.clone();
                let reply_context = msg.reply_to.map(|id| ReplyContext {
                    original_message_id: id,
                    original_text: None,
                    original_sender: None,
                });
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: sender_id,
                        display_name: msg.sender,
                        channel: ChannelId::Matrix,
                    },
                    media: msg.media,
                    reply_context,
                    origin: MessageOrigin::Matrix {
                        room_id: msg.room_id,
                        event_id: msg.event_id,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::Line(msg) => {
                let session_key = resolver.resolve_line(&msg.user_id);
                let sender_id = msg.user_id.clone();
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: sender_id,
                        display_name: msg.user_id.clone(),
                        channel: ChannelId::Line,
                    },
                    media: msg.media,
                    reply_context: None,
                    origin: MessageOrigin::Line {
                        user_id: msg.user_id,
                        reply_token: msg.reply_token,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::GoogleChat(msg) => {
                let session_key = resolver.resolve_googlechat(&msg.space_name);
                let sender_id = msg.sender_name.clone();
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: sender_id,
                        display_name: msg.sender_name,
                        channel: ChannelId::GoogleChat,
                    },
                    media: vec![],
                    reply_context: None,
                    origin: MessageOrigin::GoogleChat {
                        space_name: msg.space_name,
                        message_name: msg.message_name,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::MsTeams(msg) => {
                let session_key = resolver.resolve_msteams(&msg.tenant_id, &msg.channel_id);
                let sender_id = msg.from_user.clone();
                let reply_context = msg.reply_to.map(|id| ReplyContext {
                    original_message_id: id,
                    original_text: None,
                    original_sender: None,
                });
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: sender_id,
                        display_name: msg.from_user,
                        channel: ChannelId::MsTeams,
                    },
                    media: msg.attachments,
                    reply_context,
                    origin: MessageOrigin::MsTeams {
                        tenant_id: msg.tenant_id,
                        channel_id: msg.channel_id,
                        message_id: msg.message_id,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::Nostr(msg) => {
                let session_key = resolver.resolve_nostr(&msg.pubkey);
                let display = msg.pubkey[..8].to_string();
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: msg.pubkey.clone(),
                        display_name: display,
                        channel: ChannelId::Nostr,
                    },
                    media: vec![],
                    reply_context: None,
                    origin: MessageOrigin::Nostr {
                        pubkey: msg.pubkey,
                        event_id: msg.event_id,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::Irc(msg) => {
                let session_key = resolver.resolve_irc(&msg.server, &msg.channel);
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: msg.nick.clone(),
                        display_name: msg.nick.clone(),
                        channel: ChannelId::Irc,
                    },
                    media: vec![],
                    reply_context: None,
                    origin: MessageOrigin::Irc {
                        server: msg.server,
                        channel: msg.channel,
                        nick: msg.nick,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::Mattermost(msg) => {
                let session_key = resolver.resolve_mattermost(&msg.server_url, &msg.channel_id);
                let reply_context = msg.reply_to.map(|id| ReplyContext {
                    original_message_id: id,
                    original_text: None,
                    original_sender: None,
                });
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: msg.user_id,
                        display_name: msg.username,
                        channel: ChannelId::Mattermost,
                    },
                    media: vec![],
                    reply_context,
                    origin: MessageOrigin::Mattermost {
                        server_url: msg.server_url,
                        channel_id: msg.channel_id,
                        post_id: msg.post_id,
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
                    reply_context,
                    origin: MessageOrigin::Email {
                        message_id: msg.message_id,
                        from: msg.from,
                        to: msg.to,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::Feishu(msg) => {
                let session_key = resolver.resolve_feishu(&msg.chat_id);
                let reply_context = msg.parent_id.map(|id| ReplyContext {
                    original_message_id: id,
                    original_text: None,
                    original_sender: None,
                });
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: msg.sender_id,
                        display_name: msg.sender_name,
                        channel: ChannelId::Feishu,
                    },
                    media: vec![],
                    reply_context,
                    origin: MessageOrigin::Feishu {
                        chat_id: msg.chat_id,
                        message_id: msg.message_id,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::Twitch(msg) => {
                let session_key = resolver.resolve_twitch(&msg.channel_name);
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: msg.user_id,
                        display_name: msg.display_name,
                        channel: ChannelId::Twitch,
                    },
                    media: vec![],
                    reply_context: None,
                    origin: MessageOrigin::Twitch {
                        channel_name: msg.channel_name,
                        message_id: msg.message_id,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::NextcloudTalk(msg) => {
                let session_key = resolver.resolve_nextcloud_talk(&msg.base_url, &msg.room_token);
                let reply_context = msg.parent_id.map(|id| ReplyContext {
                    original_message_id: id.to_string(),
                    original_text: None,
                    original_sender: None,
                });
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: msg.actor_id,
                        display_name: msg.actor_display_name,
                        channel: ChannelId::NextcloudTalk,
                    },
                    media: vec![],
                    reply_context,
                    origin: MessageOrigin::NextcloudTalk {
                        base_url: msg.base_url,
                        room_token: msg.room_token,
                        message_id: msg.message_id,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::Zalo(msg) => {
                let session_key = resolver.resolve_zalo(&msg.user_id);
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: msg.user_id.clone(),
                        display_name: msg.user_id.clone(),
                        channel: ChannelId::Zalo,
                    },
                    media: msg.media,
                    reply_context: None,
                    origin: MessageOrigin::Zalo {
                        user_id: msg.user_id,
                        message_id: msg.message_id,
                    },
                    timestamp: Utc::now(),
                }
            }
            InboundMessage::Tlon(msg) => {
                let session_key = resolver.resolve_tlon(&msg.ship, &msg.channel_name);
                NormalizedMessage {
                    id: Uuid::new_v4(),
                    session_key,
                    body: msg.text,
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: msg.author.clone(),
                        display_name: msg.author,
                        channel: ChannelId::Tlon,
                    },
                    media: vec![],
                    reply_context: None,
                    origin: MessageOrigin::Tlon {
                        ship: msg.ship,
                        channel_name: msg.channel_name,
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
