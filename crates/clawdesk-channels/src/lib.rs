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
//! ├── EmailChannel       — Email via IMAP + SMTP
//! ├── IMessageChannel    — macOS iMessage (AppleScript + chat.db polling)
//! ├── IrcChannel         — IRC over TLS (SASL/NickServ auth)
//! ├── TeamsChannel       — Microsoft Teams (Bot Framework REST API)
//! ├── MatrixChannel      — Matrix protocol (Client-Server API v1.6+)
//! ├── SignalChannel      — Signal Messenger (signal-cli JSON-RPC)
//! ├── WebhookChannel     — Generic webhook (inbound POST + outbound callback)
//! ├── MastodonChannel    — Mastodon / Fediverse (ActivityPub REST API)
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

pub mod bridge_adapter;
pub mod discord;
pub mod email;
pub mod factory;
pub mod imessage;
pub mod internal;
pub mod irc;
pub mod markdown;
pub mod mastodon;
pub mod matrix;
pub mod retry_policy;
pub mod signal;
pub mod slack;
pub mod teams;
pub mod telegram;
pub mod webchat;
pub mod webhook;
pub mod whatsapp;
pub mod whatsapp_storage;
