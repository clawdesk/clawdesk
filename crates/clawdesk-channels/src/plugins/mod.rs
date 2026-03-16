//! Channel plugin adapters — wire existing channel implementations to the plugin framework.
//!
//! Each adapter wraps an existing channel and registers it with the ChannelPlugin trait
//! from `clawdesk-channel-plugins`, adding capability declarations and action handlers.

pub mod discord_plugin;
pub mod telegram_plugin;
pub mod slack_plugin;
pub mod whatsapp_plugin;
