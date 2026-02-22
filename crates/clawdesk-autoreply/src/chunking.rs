//! Reply threading persistence across streamed/split chunks.
//!
//! When a long response is split into multiple messages (Telegram's 4096-char
//! limit, Discord's 2000-char limit), all chunks must reference the same
//! `reply_to_message_id` to maintain conversational threading in group chats.
//!
//! ## Architecture
//!
//! The `ChunkEnvelope` carries metadata from the original message envelope
//! to all child chunks (decorator pattern). The splitter propagates metadata
//! with O(1) per chunk (copy a single `Option<String>` field).
//!
//! Splitting is O(n/L) where n = response length and L = platform limit.

use serde::{Deserialize, Serialize};

/// Platform message length limits.
pub mod limits {
    pub const TELEGRAM_MAX: usize = 4096;
    pub const DISCORD_MAX: usize = 2000;
    pub const SLACK_MAX: usize = 40_000;
    pub const MATRIX_MAX: usize = 65_536;
    pub const MSTEAMS_MAX: usize = 28_000;
    pub const IMESSAGE_MAX: usize = 20_000;
    pub const WHATSAPP_MAX: usize = 65_536;
    pub const DEFAULT_MAX: usize = 4096;
}

/// A chunk envelope that carries threading metadata through message splitting.
///
/// Ensures all chunks in a split delivery reference the same `reply_to` ID,
/// maintaining conversational context in group chats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkEnvelope {
    /// The chunk content text.
    pub content: String,
    /// Reply-to message ID — propagated to ALL chunks.
    pub reply_to: Option<String>,
    /// Thread ID — propagated to ALL chunks.
    pub thread_id: Option<String>,
    /// Zero-based sequence number within the split.
    pub sequence: usize,
    /// Total number of chunks in this split.
    pub total: usize,
    /// Channel-specific metadata (e.g., chat_id for Telegram).
    pub channel_id: Option<String>,
}

impl ChunkEnvelope {
    /// Create a single-chunk envelope (no splitting needed).
    pub fn single(content: String, reply_to: Option<String>, thread_id: Option<String>) -> Self {
        Self {
            content,
            reply_to,
            thread_id,
            sequence: 0,
            total: 1,
            channel_id: None,
        }
    }

    /// Whether this is the first chunk in a split.
    pub fn is_first(&self) -> bool {
        self.sequence == 0
    }

    /// Whether this is the last chunk in a split.
    pub fn is_last(&self) -> bool {
        self.sequence + 1 >= self.total
    }
}

/// Split a response into chunked envelopes with threading metadata preserved.
///
/// All chunks inherit the same `reply_to` and `thread_id` from the original
/// message, ensuring threading context is maintained across platform limits.
///
/// Splits on word boundaries when possible to avoid breaking mid-word.
///
/// ## Complexity
/// O(n/L) where n = content length and L = max_len.
pub fn split_with_threading(
    content: &str,
    max_len: usize,
    reply_to: Option<String>,
    thread_id: Option<String>,
) -> Vec<ChunkEnvelope> {
    if content.len() <= max_len {
        return vec![ChunkEnvelope::single(
            content.to_string(),
            reply_to,
            thread_id,
        )];
    }

    let chunks = split_on_boundaries(content, max_len);
    let total = chunks.len();

    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| ChunkEnvelope {
            content: chunk,
            reply_to: reply_to.clone(),
            thread_id: thread_id.clone(),
            sequence: i,
            total,
            channel_id: None,
        })
        .collect()
}

/// Split text on word/line boundaries, respecting max_len.
fn split_on_boundaries(text: &str, max_len: usize) -> Vec<String> {
    if max_len == 0 {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for line in text.split('\n') {
        // If adding this line would exceed the limit
        if !current.is_empty() && current.len() + 1 + line.len() > max_len {
            // If the current line itself exceeds max_len, hard-split it
            if line.len() > max_len {
                if !current.is_empty() {
                    chunks.push(std::mem::take(&mut current));
                }
                // Hard-split the long line
                let mut remaining = line;
                while remaining.len() > max_len {
                    // Try to split on word boundary
                    let split_at = find_word_boundary(remaining, max_len);
                    chunks.push(remaining[..split_at].to_string());
                    remaining = &remaining[split_at..];
                    // Trim leading whitespace from continuation
                    remaining = remaining.trim_start();
                }
                if !remaining.is_empty() {
                    current = remaining.to_string();
                }
            } else {
                chunks.push(std::mem::take(&mut current));
                current = line.to_string();
            }
        } else if current.is_empty() && line.len() > max_len {
            // First line (or after a flush) is itself oversized — hard-split it
            let mut remaining = line;
            while remaining.len() > max_len {
                let split_at = find_word_boundary(remaining, max_len);
                chunks.push(remaining[..split_at].to_string());
                remaining = &remaining[split_at..];
                remaining = remaining.trim_start();
            }
            if !remaining.is_empty() {
                current = remaining.to_string();
            }
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

/// Find the last word boundary before `max_pos`.
fn find_word_boundary(text: &str, max_pos: usize) -> usize {
    if max_pos >= text.len() {
        return text.len();
    }

    // Search backwards for a space or punctuation
    let bytes = text.as_bytes();
    for i in (1..=max_pos).rev() {
        if bytes[i] == b' ' || bytes[i] == b'\n' || bytes[i] == b'\t' {
            return i;
        }
    }

    // No word boundary found — hard split at max_pos
    max_pos
}

/// Get the platform-specific message limit for a channel.
pub fn limit_for_channel(channel: &str) -> usize {
    match channel.to_lowercase().as_str() {
        "telegram" => limits::TELEGRAM_MAX,
        "discord" => limits::DISCORD_MAX,
        "slack" => limits::SLACK_MAX,
        "matrix" => limits::MATRIX_MAX,
        "msteams" | "ms_teams" => limits::MSTEAMS_MAX,
        "imessage" | "bluebubbles" => limits::IMESSAGE_MAX,
        "whatsapp" => limits::WHATSAPP_MAX,
        _ => limits::DEFAULT_MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_split_needed() {
        let chunks = split_with_threading("Hello!", 100, Some("msg-1".into()), None);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, "Hello!");
        assert_eq!(chunks[0].reply_to, Some("msg-1".into()));
        assert_eq!(chunks[0].sequence, 0);
        assert_eq!(chunks[0].total, 1);
    }

    #[test]
    fn test_split_preserves_reply_to() {
        let content = "a".repeat(5000);
        let chunks = split_with_threading(
            &content,
            2000,
            Some("original-msg".into()),
            Some("thread-1".into()),
        );

        assert!(chunks.len() >= 3);
        for chunk in &chunks {
            assert_eq!(chunk.reply_to, Some("original-msg".into()));
            assert_eq!(chunk.thread_id, Some("thread-1".into()));
            assert!(chunk.content.len() <= 2000);
        }
        assert_eq!(chunks[0].sequence, 0);
        assert_eq!(chunks.last().unwrap().sequence, chunks.len() - 1);
        assert!(chunks[0].is_first());
        assert!(chunks.last().unwrap().is_last());
    }

    #[test]
    fn test_split_on_line_boundaries() {
        let content = "Line 1\nLine 2\nLine 3\nLine 4\nLine 5";
        let chunks = split_with_threading(content, 20, None, None);
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(chunk.content.len() <= 20);
        }
    }

    #[test]
    fn test_limit_for_channel() {
        assert_eq!(limit_for_channel("telegram"), 4096);
        assert_eq!(limit_for_channel("Discord"), 2000);
        assert_eq!(limit_for_channel("slack"), 40_000);
        assert_eq!(limit_for_channel("unknown"), 4096);
    }

    #[test]
    fn test_chunk_envelope_flags() {
        let chunk = ChunkEnvelope {
            content: "test".into(),
            reply_to: None,
            thread_id: None,
            sequence: 0,
            total: 3,
            channel_id: None,
        };
        assert!(chunk.is_first());
        assert!(!chunk.is_last());

        let last = ChunkEnvelope {
            sequence: 2,
            total: 3,
            ..chunk.clone()
        };
        assert!(!last.is_first());
        assert!(last.is_last());
    }
}
