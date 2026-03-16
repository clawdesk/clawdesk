//! Channel-specific poll adapters — translate abstract polls to native platform APIs.

pub mod telegram_polls;
pub mod discord_polls;
pub mod slack_polls;
pub mod renderer;
