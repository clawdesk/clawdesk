//! # clawdesk-channels
//!
//! Concrete channel implementations for ClawDesk.
//!
//! Each module implements the `Channel` trait (and optional capability
//! traits like `Streaming`, `Reactions`, etc.) for a specific messaging
//! platform.
//!
//! ## Channel hierarchy
//!
//! ```text
//! Channel (Layer 0 — required)
//! ├── WebChatChannel     — Gateway WebSocket (simplest, always available)
//! ├── TelegramChannel    — Telegram Bot API (long-polling)
//! ├── DiscordChannel     — Discord Bot API (WebSocket gateway)
//! ├── SlackChannel       — Slack Bot (Socket Mode / Events API)
//! └── InternalChannel    — In-process testing channel
//! ```
//!
//! ## Invariant: Every channel is a bidirectional functor
//!
//! ```text
//! F: PlatformMsg → NormalizedMessage    (inbound normalization)
//! G: OutboundMessage → PlatformApiCall  (outbound rendering)
//!
//! Correctness: G ∘ F ∘ G⁻¹ ∘ F⁻¹ ≈ id  (roundtrip fidelity)
//! ```

pub mod discord;
pub mod email;
pub mod factory;
pub mod feishu;
pub mod googlechat;
pub mod imessage;
pub mod internal;
pub mod irc;
pub mod line;
pub mod matrix;
pub mod mattermost;
pub mod msteams;
pub mod nextcloud_talk;
pub mod nostr;
pub mod retry_policy;
pub mod sidecar;
pub mod signal;
pub mod slack;
pub mod telegram;
pub mod tlon;
pub mod twitch;
pub mod markdown;
pub mod bluebubbles;
pub mod webchat;
pub mod whatsapp;
pub mod zalo;
pub mod zalouser;
