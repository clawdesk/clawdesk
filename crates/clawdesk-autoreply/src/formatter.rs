//! Response formatter — adapts agent output to channel constraints.
//!
//! Handles max message length, markdown support, media attachments,
//! code block splitting, and channel-specific formatting.

use clawdesk_types::channel::ChannelId;
use serde::{Deserialize, Serialize};

/// Constraints for a specific channel's message format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConstraints {
    /// Maximum message length in characters.
    pub max_length: usize,
    /// Whether Markdown formatting is supported.
    pub supports_markdown: bool,
    /// Whether HTML formatting is supported.
    pub supports_html: bool,
    /// Whether code blocks are supported.
    pub supports_code_blocks: bool,
    /// Whether media attachments are supported.
    pub supports_media: bool,
    /// Maximum number of parts a long message can be split into.
    pub max_split_parts: usize,
    /// Optional prefix for continuation messages.
    pub continuation_prefix: Option<String>,
}

impl ChannelConstraints {
    /// Returns constraints for a channel based on its ID.
    pub fn for_channel(channel: &ChannelId) -> Self {
        match channel {
            ChannelId::Telegram => Self {
                max_length: 4096,
                supports_markdown: true,
                supports_html: true,
                supports_code_blocks: true,
                supports_media: true,
                max_split_parts: 10,
                continuation_prefix: None,
            },
            ChannelId::Discord => Self {
                max_length: 2000,
                supports_markdown: true,
                supports_html: false,
                supports_code_blocks: true,
                supports_media: true,
                max_split_parts: 5,
                continuation_prefix: None,
            },
            ChannelId::Slack => Self {
                max_length: 4000,
                supports_markdown: true,
                supports_html: false,
                supports_code_blocks: true,
                supports_media: true,
                max_split_parts: 10,
                continuation_prefix: None,
            },
            ChannelId::WhatsApp => Self {
                max_length: 4096,
                supports_markdown: true,
                supports_html: false,
                supports_code_blocks: false,
                supports_media: true,
                max_split_parts: 5,
                continuation_prefix: None,
            },
            ChannelId::Internal | ChannelId::WebChat => Self {
                max_length: 65536,
                supports_markdown: false,
                supports_html: false,
                supports_code_blocks: false,
                supports_media: true,
                max_split_parts: 1,
                continuation_prefix: None,
            },
            _ => Self {
                max_length: 4096,
                supports_markdown: true,
                supports_html: false,
                supports_code_blocks: true,
                supports_media: true,
                max_split_parts: 5,
                continuation_prefix: None,
            },
        }
    }
}

/// A formatted message segment ready for delivery.
///
/// Carries the full payload needed by channel adapters: text, media
/// attachments, threading metadata, voice hints, and error flags.
/// Zero information loss at the formatting boundary.
#[derive(Debug, Clone)]
pub struct FormattedSegment {
    /// The text content.
    pub text: String,
    /// Part number (1-indexed).
    pub part: usize,
    /// Total number of parts.
    pub total_parts: usize,
    /// Media attachment URLs for this segment.
    /// Populated on the first segment when media accompanies the response.
    pub media_urls: Vec<String>,
    /// Reply-to message ID for threading support.
    pub reply_to: Option<String>,
    /// Whether audio media should be sent as a voice message.
    pub audio_as_voice: bool,
    /// Whether this segment represents an error message.
    pub is_error: bool,
}

/// Formats agent responses to fit channel constraints.
pub struct ResponseFormatter;

impl ResponseFormatter {
    /// Format a response body for a specific channel.
    pub fn format(body: &str, channel: &ChannelId) -> Vec<FormattedSegment> {
        let constraints = ChannelConstraints::for_channel(channel);
        let processed = Self::process_formatting(body, &constraints);
        Self::split_to_segments(&processed, &constraints)
    }

    /// Format a response with full metadata propagation.
    ///
    /// Media URLs are attached to the first segment (channels send media
    /// before/with the first text chunk). Reply-to and voice hints are
    /// propagated to all segments for threading consistency.
    pub fn format_with_media(
        body: &str,
        channel: &ChannelId,
        media_urls: Vec<String>,
        reply_to: Option<String>,
        audio_as_voice: bool,
        is_error: bool,
    ) -> Vec<FormattedSegment> {
        let mut segments = Self::format(body, channel);

        // Propagate metadata to all segments
        for (i, seg) in segments.iter_mut().enumerate() {
            // Media attached to first segment only
            if i == 0 {
                seg.media_urls = media_urls.clone();
            }
            seg.reply_to = reply_to.clone();
            seg.audio_as_voice = audio_as_voice;
            seg.is_error = is_error;
        }

        segments
    }

    /// Format a single streaming chunk for a specific channel.
    ///
    /// Per-chunk formatting for the unified streaming pipeline. Applies
    /// channel-specific formatting without splitting (chunks are already
    /// appropriately sized from the LLM). Sub-microsecond overhead.
    pub fn format_chunk(chunk: &str, channel: &ChannelId) -> String {
        let constraints = ChannelConstraints::for_channel(channel);
        Self::process_formatting(chunk, &constraints)
    }

    /// Process formatting based on channel capabilities.
    fn process_formatting(body: &str, constraints: &ChannelConstraints) -> String {
        let mut result = body.to_string();

        if !constraints.supports_markdown {
            result = Self::strip_markdown(&result);
        }
        if !constraints.supports_code_blocks {
            result = Self::strip_code_blocks(&result);
        }
        result
    }

    /// Strip Markdown formatting, preserving the text content.
    fn strip_markdown(text: &str) -> String {
        let mut result = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();

        while let Some(ch) = chars.next() {
            match ch {
                '*' | '_' => {
                    // Skip bold/italic markers.
                    if chars.peek() == Some(&ch) {
                        chars.next();
                    }
                }
                '#' => {
                    // Skip heading markers and the space after.
                    while chars.peek() == Some(&'#') {
                        chars.next();
                    }
                    if chars.peek() == Some(&' ') {
                        chars.next();
                    }
                }
                '`' => {
                    // Keep code content, strip backticks.
                    if chars.peek() == Some(&'`') {
                        // Triple backtick block — skip opening.
                        chars.next();
                        if chars.peek() == Some(&'`') {
                            chars.next();
                        }
                        // Skip language identifier on same line.
                        while let Some(&c) = chars.peek() {
                            if c == '\n' {
                                break;
                            }
                            chars.next();
                        }
                    }
                }
                _ => result.push(ch),
            }
        }
        result
    }

    /// Strip code block fences but keep the code content.
    fn strip_code_blocks(text: &str) -> String {
        let mut result = String::with_capacity(text.len());
        let mut in_code_block = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("```") {
                in_code_block = !in_code_block;
                if in_code_block {
                    continue; // Skip opening fence.
                } else {
                    continue; // Skip closing fence.
                }
            }
            result.push_str(line);
            result.push('\n');
        }
        // Remove trailing newline if original didn't end with one.
        if !text.ends_with('\n') && result.ends_with('\n') {
            result.pop();
        }
        result
    }

    /// Split a processed message into segments that fit the channel constraints.
    fn split_to_segments(text: &str, constraints: &ChannelConstraints) -> Vec<FormattedSegment> {
        if text.len() <= constraints.max_length {
            return vec![FormattedSegment {
                text: text.to_string(),
                part: 1,
                total_parts: 1,
                media_urls: Vec::new(),
                reply_to: None,
                audio_as_voice: false,
                is_error: false,
            }];
        }

        let mut segments = Vec::new();
        let mut remaining = text;

        while !remaining.is_empty() && segments.len() < constraints.max_split_parts {
            if remaining.len() <= constraints.max_length {
                segments.push(remaining.to_string());
                break;
            }

            // Find a good split point (prefer paragraph, then line, then word boundary).
            let max = constraints.max_length;
            let chunk = &remaining[..max];

            let split_at = chunk
                .rfind("\n\n")
                .or_else(|| chunk.rfind('\n'))
                .or_else(|| chunk.rfind(' '))
                .unwrap_or(max);

            let split_at = if split_at == 0 { max } else { split_at };

            segments.push(remaining[..split_at].to_string());
            remaining = remaining[split_at..].trim_start();
        }

        // If there's still remaining text, append to last segment.
        if !remaining.is_empty() && !segments.is_empty() {
            let last = segments.last_mut().unwrap();
            last.push_str("\n…");
        }

        let total = segments.len();
        segments
            .into_iter()
            .enumerate()
            .map(|(i, text)| FormattedSegment {
                text,
                part: i + 1,
                total_parts: total,
                media_urls: Vec::new(),
                reply_to: None,
                audio_as_voice: false,
                is_error: false,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_message_no_split() {
        let segments = ResponseFormatter::format("Hello!", &ChannelId::Telegram);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "Hello!");
        assert_eq!(segments[0].total_parts, 1);
    }

    #[test]
    fn test_long_message_splits() {
        let long = "a ".repeat(1200); // 2400 chars.
        let segments = ResponseFormatter::format(&long, &ChannelId::Discord);
        assert!(segments.len() > 1);
        for seg in &segments {
            assert!(seg.text.len() <= 2000 + 2); // +2 for trailing ellipsis.
        }
    }

    #[test]
    fn test_plain_text_stripped() {
        let md = "**bold** and _italic_ and ## heading";
        let segments = ResponseFormatter::format(md, &ChannelId::Internal);
        assert_eq!(segments.len(), 1);
        assert!(!segments[0].text.contains("**"));
        assert!(!segments[0].text.contains("##"));
    }

    #[test]
    fn test_code_blocks_stripped_for_whatsapp() {
        let code = "before\n```rust\nfn main() {}\n```\nafter";
        let segments = ResponseFormatter::format(code, &ChannelId::WhatsApp);
        assert_eq!(segments.len(), 1);
        assert!(segments[0].text.contains("fn main()"));
        assert!(!segments[0].text.contains("```"));
    }

    #[test]
    fn test_constraints_for_channels() {
        assert_eq!(
            ChannelConstraints::for_channel(&ChannelId::Discord).max_length,
            2000
        );
        assert_eq!(
            ChannelConstraints::for_channel(&ChannelId::Telegram).max_length,
            4096
        );
        assert!(!ChannelConstraints::for_channel(&ChannelId::Internal).supports_markdown);
    }
}
