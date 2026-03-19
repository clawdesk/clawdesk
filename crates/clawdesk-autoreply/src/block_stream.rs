//! Block streaming pipeline — multi-block response delivery with coalescing.
//!
//! Handles streaming responses containing multiple content types:
//! text, code, media, tool-use, and TTS blocks. Each block type has
//! different delivery semantics.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// A typed content block in a streaming response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    /// Streaming text (delivered character-by-character).
    Text { content: String },
    /// Code block (delivered on completion).
    Code { language: String, content: String },
    /// Media attachment.
    Media { path: String, mime_type: String },
    /// Tool use block.
    ToolUse { tool_name: String, input: serde_json::Value },
    /// Tool result block.
    ToolResult { tool_name: String, output: String, is_error: bool },
    /// TTS audio chunk.
    Tts { audio_url: String },
}

/// Coalesced blocks ready for delivery.
#[derive(Debug, Clone)]
pub struct CoalescedDelivery {
    pub blocks: Vec<Block>,
    pub merged_text: Option<String>,
}

/// Block coalescer — merges adjacent text blocks within a time window.
///
/// If blocks[i] and blocks[i+1] are both Text and arrive within window_ms,
/// merge into one delivery. Reduces API calls by merge_factor ≈ 3-5.
pub struct BlockCoalescer {
    window: Duration,
    pending: Vec<(Block, Instant)>,
}

impl BlockCoalescer {
    pub fn new(window_ms: u64) -> Self {
        Self {
            window: Duration::from_millis(window_ms),
            pending: Vec::new(),
        }
    }

    /// Push a block into the coalescer.
    pub fn push(&mut self, block: Block) {
        self.pending.push((block, Instant::now()));
    }

    /// Flush blocks that are ready for delivery (window expired or non-text).
    pub fn flush(&mut self) -> Vec<CoalescedDelivery> {
        if self.pending.is_empty() {
            return vec![];
        }

        let now = Instant::now();
        let mut deliveries = Vec::new();
        let mut text_buffer = String::new();
        let mut non_text_queue: Vec<Block> = Vec::new();

        let pending = std::mem::take(&mut self.pending);
        for (block, arrived) in pending {
            match &block {
                Block::Text { content } if now.duration_since(arrived) >= self.window => {
                    // Window expired — flush accumulated text.
                    text_buffer.push_str(content);
                }
                Block::Text { content } => {
                    // Still within window — accumulate.
                    text_buffer.push_str(content);
                    // Re-queue with original timestamp.
                    self.pending.push((Block::Text { content: String::new() }, arrived));
                    continue;
                }
                _ => {
                    // Non-text block — flush any accumulated text first.
                    if !text_buffer.is_empty() {
                        deliveries.push(CoalescedDelivery {
                            blocks: vec![Block::Text { content: text_buffer.clone() }],
                            merged_text: Some(text_buffer.clone()),
                        });
                        text_buffer.clear();
                    }
                    non_text_queue.push(block);
                }
            }
        }

        // Flush remaining text.
        if !text_buffer.is_empty() {
            deliveries.push(CoalescedDelivery {
                blocks: vec![Block::Text { content: text_buffer.clone() }],
                merged_text: Some(text_buffer),
            });
        }

        // Deliver non-text blocks individually.
        for block in non_text_queue {
            deliveries.push(CoalescedDelivery {
                blocks: vec![block],
                merged_text: None,
            });
        }

        deliveries
    }

    /// Force-flush all pending blocks.
    pub fn flush_all(&mut self) -> Vec<CoalescedDelivery> {
        // Treat everything as expired.
        let pending = std::mem::take(&mut self.pending);
        let mut text_buffer = String::new();
        let mut deliveries = Vec::new();

        for (block, _) in pending {
            match block {
                Block::Text { content } => text_buffer.push_str(&content),
                other => {
                    if !text_buffer.is_empty() {
                        deliveries.push(CoalescedDelivery {
                            blocks: vec![Block::Text { content: text_buffer.clone() }],
                            merged_text: Some(text_buffer.clone()),
                        });
                        text_buffer.clear();
                    }
                    deliveries.push(CoalescedDelivery { blocks: vec![other], merged_text: None });
                }
            }
        }
        if !text_buffer.is_empty() {
            deliveries.push(CoalescedDelivery {
                blocks: vec![Block::Text { content: text_buffer.clone() }],
                merged_text: Some(text_buffer),
            });
        }
        deliveries
    }
}

/// Heartbeat typing indicator scheduler.
///
/// Maintains per-channel refresh intervals because each platform requires
/// different typing indicator cadences:
/// - Discord: refresh every 10s (typing status expires after 10s)
/// - Telegram: refresh every 5s (upload_chat_action lasts ~5s)
/// - Slack: refresh every 3s
/// - WhatsApp: single indicator per message (no refresh needed)
/// - Default: 8s
pub struct TypingHeartbeat {
    /// Default interval for channels without a specific override.
    pub default_interval: Duration,
    /// Per-channel interval overrides.
    channel_intervals: std::collections::HashMap<String, Duration>,
}

impl TypingHeartbeat {
    pub fn new(default_interval_secs: u64) -> Self {
        Self {
            default_interval: Duration::from_secs(default_interval_secs),
            channel_intervals: Self::default_channel_intervals(),
        }
    }

    /// Get the typing indicator refresh interval for a specific channel.
    pub fn interval_for(&self, channel: &str) -> Duration {
        self.channel_intervals
            .get(&channel.to_lowercase())
            .copied()
            .unwrap_or(self.default_interval)
    }

    /// Override the interval for a specific channel.
    pub fn set_channel_interval(&mut self, channel: &str, interval: Duration) {
        self.channel_intervals.insert(channel.to_lowercase(), interval);
    }

    /// Whether a channel needs periodic typing refresh (false = fire-and-forget).
    pub fn needs_refresh(&self, channel: &str) -> bool {
        // WhatsApp and email don't need periodic refresh.
        !matches!(channel.to_lowercase().as_str(), "whatsapp" | "email" | "sms")
    }

    fn default_channel_intervals() -> std::collections::HashMap<String, Duration> {
        let mut m = std::collections::HashMap::new();
        m.insert("discord".to_string(), Duration::from_secs(10));
        m.insert("telegram".to_string(), Duration::from_secs(5));
        m.insert("slack".to_string(), Duration::from_secs(3));
        m.insert("matrix".to_string(), Duration::from_secs(8));
        m.insert("teams".to_string(), Duration::from_secs(5));
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalescer_merges_text() {
        let mut c = BlockCoalescer::new(0); // 0ms window = immediate flush
        c.push(Block::Text { content: "hello ".into() });
        c.push(Block::Text { content: "world".into() });
        let deliveries = c.flush();
        // With 0ms window, both should be flushed.
        assert!(!deliveries.is_empty());
    }

    #[test]
    fn coalescer_separates_non_text() {
        let mut c = BlockCoalescer::new(0);
        c.push(Block::Text { content: "start".into() });
        c.push(Block::Code { language: "rust".into(), content: "fn main(){}".into() });
        c.push(Block::Text { content: "end".into() });
        let deliveries = c.flush_all();
        assert!(deliveries.len() >= 2); // at least text + code
    }

    #[test]
    fn typing_heartbeat_per_channel() {
        let hb = TypingHeartbeat::new(8);
        assert_eq!(hb.interval_for("discord"), Duration::from_secs(10));
        assert_eq!(hb.interval_for("telegram"), Duration::from_secs(5));
        assert_eq!(hb.interval_for("slack"), Duration::from_secs(3));
        // Unknown channel uses default.
        assert_eq!(hb.interval_for("irc"), Duration::from_secs(8));
    }

    #[test]
    fn typing_heartbeat_no_refresh_for_whatsapp() {
        let hb = TypingHeartbeat::new(8);
        assert!(!hb.needs_refresh("whatsapp"));
        assert!(!hb.needs_refresh("email"));
        assert!(hb.needs_refresh("discord"));
        assert!(hb.needs_refresh("telegram"));
    }
}
