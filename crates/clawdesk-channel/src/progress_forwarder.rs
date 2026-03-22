//! Progress forwarder — pushes agent lifecycle events to linked channels.
//!
//! ## Scenario
//!
//! User links their Telegram to their desktop ClawDesk session:
//!
//! ```text
//! ┌──────────────┐    AgentEvent stream    ┌──────────────────┐
//! │ Desktop      │ ──────────────────────► │ ProgressForwarder │
//! │ (Agent runs) │                         │                   │
//! └──────────────┘                         │ 1. Resolve user   │
//!                                          │ 2. Find notify    │
//!                                          │    channels       │
//!                                          │ 3. Format update  │
//!                                          │ 4. Channel.send() │
//!                                          └────────┬──────────┘
//!                                                   │
//!                                                   ▼
//!                                          ┌──────────────────┐
//!                                          │ Telegram Bot     │
//!                                          │ (user's phone)   │
//!                                          └──────────────────┘
//! ```
//!
//! ## Event Compression
//!
//! Not every `AgentEvent` should be forwarded. We compress the event
//! stream into meaningful status updates:
//!
//! - `RoundStart` → "🔄 Starting round {n}..."
//! - `ToolStart` → "🔧 Using tool: {name}"
//! - `ToolEnd` → "✅ {name} completed ({duration}ms)"
//! - `StreamChunk` → debounced, edit-in-place if channel supports streaming
//! - `Done` → "✨ Completed ({n} rounds)"
//! - `Error` → "❌ Error: {msg}"
//!
//! `MessageUpdate`, `ThinkingChunk`, and other high-frequency events
//! are NOT forwarded individually — they'd flood the channel.

use crate::session_bridge::SessionBridge;
use crate::Channel;
use clawdesk_types::channel::ChannelId;
use clawdesk_types::message::{MessageOrigin, OutboundMessage};
use clawdesk_types::session::SessionKey;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, warn};

/// Agent event summary — compressed view of lifecycle events.
///
/// Only significant state transitions are forwarded. High-frequency
/// streaming deltas are absorbed.
#[derive(Debug, Clone)]
pub enum ProgressUpdate {
    /// Agent started processing
    Started {
        session_id: String,
        model: String,
    },
    /// New round of tool calls
    RoundStarted {
        round: usize,
    },
    /// Tool execution began
    ToolStarted {
        name: String,
        args_preview: String,
    },
    /// Tool execution completed
    ToolCompleted {
        name: String,
        success: bool,
        duration_ms: u64,
    },
    /// Tool was blocked by policy
    ToolBlocked {
        name: String,
        reason: String,
    },
    /// Response text (final, not streaming deltas)
    ResponseReady {
        preview: String,
    },
    /// Agent completed all rounds
    Completed {
        total_rounds: usize,
    },
    /// Agent encountered an error
    Error {
        message: String,
    },
}

impl ProgressUpdate {
    /// Format as a compact status line for Telegram/mobile consumption.
    ///
    /// Uses emoji for visual scanning on small screens.
    pub fn to_status_line(&self) -> String {
        match self {
            Self::Started { model, .. } => {
                format!("🧠 Processing with {model}...")
            }
            Self::RoundStarted { round } => {
                format!("🔄 Round {round}")
            }
            Self::ToolStarted { name, args_preview } => {
                let preview = if args_preview.len() > 60 {
                    format!("{}…", &args_preview[..60])
                } else {
                    args_preview.clone()
                };
                format!("🔧 {name}: {preview}")
            }
            Self::ToolCompleted {
                name,
                success,
                duration_ms,
            } => {
                let icon = if *success { "✅" } else { "❌" };
                format!("{icon} {name} ({duration_ms}ms)")
            }
            Self::ToolBlocked { name, reason } => {
                format!("🚫 {name} blocked: {reason}")
            }
            Self::ResponseReady { preview } => {
                let truncated = if preview.len() > 200 {
                    format!("{}…", &preview[..200])
                } else {
                    preview.clone()
                };
                format!("💬 Response ready:\n{truncated}")
            }
            Self::Completed { total_rounds } => {
                format!("✨ Done ({total_rounds} rounds)")
            }
            Self::Error { message } => {
                let truncated = if message.len() > 200 {
                    format!("{}…", &message[..200])
                } else {
                    message.clone()
                };
                format!("❌ Error: {truncated}")
            }
        }
    }
}

/// Channel registry — holds references to active channel instances by ID.
///
/// The forwarder needs to call `channel.send()` on the notification channels.
/// This registry maps `ChannelId` → `Arc<dyn Channel>` for dispatch.
pub struct ChannelRegistry {
    channels: HashMap<ChannelId, Arc<dyn Channel>>,
}

impl ChannelRegistry {
    pub fn new() -> Self {
        Self {
            channels: HashMap::new(),
        }
    }

    pub fn register(&mut self, id: ChannelId, channel: Arc<dyn Channel>) {
        self.channels.insert(id, channel);
    }

    pub fn get(&self, id: ChannelId) -> Option<&Arc<dyn Channel>> {
        self.channels.get(&id)
    }
}

/// Progress forwarder configuration.
#[derive(Debug, Clone)]
pub struct ForwarderConfig {
    /// Whether to forward tool start/end events (verbose mode).
    pub forward_tools: bool,
    /// Whether to forward round start events.
    pub forward_rounds: bool,
    /// Whether to include a response preview in completion notification.
    pub include_response_preview: bool,
    /// Maximum response preview length (chars).
    pub max_preview_len: usize,
}

impl Default for ForwarderConfig {
    fn default() -> Self {
        Self {
            forward_tools: true,
            forward_rounds: false,
            include_response_preview: true,
            max_preview_len: 200,
        }
    }
}

/// Send a progress update to all notification channels for a session.
///
/// This is the core dispatch function. It:
/// 1. Looks up the user who owns the session via `SessionBridge`
/// 2. Gets their `notify_channels`
/// 3. Formats the update for each channel
/// 4. Sends via `Channel::send()`
pub async fn dispatch_progress(
    update: &ProgressUpdate,
    session_key: &SessionKey,
    bridge: &SessionBridge,
    channels: &ChannelRegistry,
) {
    let notify = bridge.notification_channels_for_session(session_key);
    if notify.is_empty() {
        return;
    }

    let status_line = update.to_status_line();
    debug!(
        session = %session_key,
        targets = notify.len(),
        "Forwarding progress to notification channels"
    );

    for target in &notify {
        let channel = match channels.get(target.channel) {
            Some(c) => c,
            None => {
                warn!(
                    channel = %target.channel,
                    "Notification channel not in registry — skipping"
                );
                continue;
            }
        };

        let origin = match target.channel {
            ChannelId::Telegram => MessageOrigin::Telegram {
                chat_id: target.identifier.parse::<i64>().unwrap_or(0),
                message_id: 0,
                thread_id: None,
            },
            ChannelId::Discord => MessageOrigin::Discord {
                guild_id: 0,
                channel_id: target.identifier.parse::<u64>().unwrap_or(0),
                message_id: 0,
                is_dm: true,
                thread_id: None,
            },
            ChannelId::Slack => MessageOrigin::Slack {
                team_id: String::new(),
                channel_id: target.identifier.clone(),
                user_id: String::new(),
                ts: String::new(),
                thread_ts: None,
            },
            _ => MessageOrigin::Internal {
                source: "progress_forwarder".to_string(),
            },
        };

        let msg = OutboundMessage {
            origin,
            body: status_line.clone(),
            media: Vec::new(),
            reply_to: None,
            thread_id: None,
        };

        if let Err(e) = channel.send(msg).await {
            warn!(
                channel = %target.channel,
                error = %e,
                "Failed to send progress update"
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line_formatting() {
        let update = ProgressUpdate::Started {
            session_id: "test".into(),
            model: "claude-4-sonnet".into(),
        };
        assert!(update.to_status_line().contains("claude-4-sonnet"));

        let update = ProgressUpdate::ToolStarted {
            name: "bash".into(),
            args_preview: "ls -la".into(),
        };
        assert!(update.to_status_line().contains("bash"));
        assert!(update.to_status_line().contains("ls -la"));

        let update = ProgressUpdate::Completed { total_rounds: 3 };
        assert!(update.to_status_line().contains("3 rounds"));
    }

    #[test]
    fn long_preview_truncated() {
        let long_text = "x".repeat(500);
        let update = ProgressUpdate::ResponseReady {
            preview: long_text,
        };
        let line = update.to_status_line();
        assert!(line.len() < 300); // Truncated + emoji + label
    }

    #[test]
    fn tool_args_truncated() {
        let long_args = "a".repeat(200);
        let update = ProgressUpdate::ToolStarted {
            name: "bash".into(),
            args_preview: long_args,
        };
        let line = update.to_status_line();
        assert!(line.contains("…")); // Truncation marker
    }
}
