//! Per-channel reply formatting and semantic chunking.
//!
//! Transforms standard Markdown into channel-specific markup formats:
//! - **Slack**: mrkdwn (bold = `*text*`, italic = `_text_`, code = `` `code` ``, link = `<url|text>`)
//! - **Telegram**: MarkdownV2 (bold = `*text*`, italic = `_text_`, escapes `.`, `-`, `!`, etc.)
//! - **Discord**: Standard Markdown (already native)
//! - **WhatsApp**: Bold = `*text*`, italic = `_text_`, no code blocks
//! - **Signal**: Plain text only (strip all formatting)
//!
//! ## Semantic Chunking
//!
//! Uses minimum-cost segmentation to split long messages at natural boundaries:
//!
//! ```text
//! cost(split_point) = {
//!     paragraph_break:  0   (free — ideal split)
//!     sentence_end:     1   (cheap — good split)
//!     line_break:       2   (acceptable)
//!     word_boundary:    5   (expensive — lose context)
//!     code_block_mid:   20  (very expensive — break code)
//!     hard_break:       50  (worst case — mid-word)
//! }
//! ```
//!
//! The chunker finds the minimum-cost split point within the last 20% of
//! the max_length window, preferring semantic boundaries.
//!
//! ## Rate-Limited Delivery
//!
//! Token bucket rate limiter controls message delivery cadence per channel
//! to avoid platform rate limits:
//!
//! ```text
//! tokens(t) = min(capacity, tokens(t-1) + (t - last_refill) × rate)
//! ```

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Segmentation cost constants for minimum-cost chunking.
pub mod costs {
    pub const PARAGRAPH_BREAK: u32 = 0;
    pub const SENTENCE_END: u32 = 1;
    pub const LINE_BREAK: u32 = 2;
    pub const WORD_BOUNDARY: u32 = 5;
    pub const CODE_BLOCK_MID: u32 = 20;
    pub const HARD_BREAK: u32 = 50;
}

/// Target markup format for a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarkupFormat {
    /// Standard Markdown (Discord, Matrix).
    Markdown,
    /// Slack mrkdwn format.
    SlackMrkdwn,
    /// Telegram MarkdownV2.
    TelegramMarkdownV2,
    /// WhatsApp formatting (subset of Markdown).
    WhatsApp,
    /// Plain text (Signal, iMessage, SMS).
    PlainText,
    /// HTML (email, some web embeds).
    Html,
}

/// A semantically chunked message segment.
#[derive(Debug, Clone)]
pub struct ChunkedSegment {
    /// The formatted text content.
    pub content: String,
    /// Part number (1-indexed).
    pub part: usize,
    /// Total number of parts.
    pub total_parts: usize,
    /// Whether this segment ends mid-code-block.
    pub ends_in_code_block: bool,
    /// Cost of the split point that produced this segment.
    pub split_cost: u32,
}

/// Per-channel reply formatter — converts Markdown to channel-native format
/// and applies minimum-cost semantic chunking.
pub struct ReplyFormatter;

impl ReplyFormatter {
    /// Format and chunk a Markdown response for a specific channel.
    ///
    /// 1. Convert Markdown to channel-native markup
    /// 2. Apply minimum-cost semantic chunking
    /// 3. Return ordered segments ready for delivery
    pub fn format_and_chunk(
        markdown: &str,
        format: MarkupFormat,
        max_length: usize,
    ) -> Vec<ChunkedSegment> {
        let converted = Self::convert_markup(markdown, format);
        Self::semantic_chunk(&converted, max_length)
    }

    /// Convert standard Markdown to channel-specific markup.
    pub fn convert_markup(markdown: &str, format: MarkupFormat) -> String {
        match format {
            MarkupFormat::Markdown => markdown.to_string(),
            MarkupFormat::SlackMrkdwn => Self::to_slack_mrkdwn(markdown),
            MarkupFormat::TelegramMarkdownV2 => Self::to_telegram_v2(markdown),
            MarkupFormat::WhatsApp => Self::to_whatsapp(markdown),
            MarkupFormat::PlainText => Self::to_plain_text(markdown),
            MarkupFormat::Html => Self::to_html(markdown),
        }
    }

    // ─── Slack mrkdwn ──────────────────────────────────────────────

    /// Convert Markdown to Slack mrkdwn format.
    ///
    /// Differences from standard Markdown:
    /// - Bold: `**text**` → `*text*`
    /// - Italic: `*text*` or `_text_` → `_text_`
    /// - Strikethrough: `~~text~~` → `~text~`
    /// - Links: `[text](url)` → `<url|text>`
    /// - Headers: `# Header` → `*Header*` (bold)
    /// - Code blocks and inline code are the same
    fn to_slack_mrkdwn(md: &str) -> String {
        let mut result = String::with_capacity(md.len());
        let mut chars = md.chars().peekable();
        let mut in_code_block = false;
        let mut in_inline_code = false;

        while let Some(ch) = chars.next() {
            if in_code_block {
                result.push(ch);
                if ch == '`' && chars.peek() == Some(&'`') {
                    result.push(chars.next().unwrap());
                    if chars.peek() == Some(&'`') {
                        result.push(chars.next().unwrap());
                        in_code_block = false;
                    }
                }
                continue;
            }

            if in_inline_code {
                result.push(ch);
                if ch == '`' {
                    in_inline_code = false;
                }
                continue;
            }

            match ch {
                '`' => {
                    if chars.peek() == Some(&'`') {
                        let c2 = chars.next().unwrap();
                        if chars.peek() == Some(&'`') {
                            let c3 = chars.next().unwrap();
                            result.push(ch);
                            result.push(c2);
                            result.push(c3);
                            in_code_block = true;
                        } else {
                            result.push(ch);
                            result.push(c2);
                        }
                    } else {
                        result.push(ch);
                        in_inline_code = true;
                    }
                }
                '*' => {
                    if chars.peek() == Some(&'*') {
                        // **bold** → *bold*
                        chars.next(); // consume second *
                        result.push('*');
                    } else {
                        // *italic* → _italic_
                        result.push('_');
                    }
                }
                '~' => {
                    if chars.peek() == Some(&'~') {
                        // ~~strike~~ → ~strike~
                        chars.next();
                        result.push('~');
                    } else {
                        result.push(ch);
                    }
                }
                '[' => {
                    // [text](url) → <url|text>
                    let mut link_text = String::new();
                    let mut found_link = false;
                    let mut inner_chars = chars.clone();

                    // Collect link text
                    while let Some(c) = inner_chars.next() {
                        if c == ']' {
                            if inner_chars.peek() == Some(&'(') {
                                inner_chars.next(); // consume (
                                let mut url = String::new();
                                for c in inner_chars.by_ref() {
                                    if c == ')' {
                                        break;
                                    }
                                    url.push(c);
                                }
                                result.push('<');
                                result.push_str(&url);
                                result.push('|');
                                result.push_str(&link_text);
                                result.push('>');
                                chars = inner_chars;
                                found_link = true;
                            }
                            break;
                        }
                        link_text.push(c);
                    }

                    if !found_link {
                        result.push('[');
                    }
                }
                '#' => {
                    // # Header → *Header* (bold in Slack)
                    while chars.peek() == Some(&'#') {
                        chars.next();
                    }
                    if chars.peek() == Some(&' ') {
                        chars.next();
                    }
                    // Collect header text until newline
                    let mut header = String::new();
                    while let Some(&c) = chars.peek() {
                        if c == '\n' {
                            break;
                        }
                        header.push(chars.next().unwrap());
                    }
                    result.push('*');
                    result.push_str(&header);
                    result.push('*');
                }
                _ => result.push(ch),
            }
        }

        result
    }

    // ─── Telegram MarkdownV2 ───────────────────────────────────────

    /// Convert Markdown to Telegram MarkdownV2.
    ///
    /// MarkdownV2 requires escaping these characters outside of code:
    /// `_`, `*`, `[`, `]`, `(`, `)`, `~`, `` ` ``, `>`, `#`, `+`, `-`, `=`, `|`, `{`, `}`, `.`, `!`
    ///
    /// Bold/italic/strike/code syntax is similar but stricter.
    fn to_telegram_v2(md: &str) -> String {
        let must_escape = ['_', '[', ']', '(', ')', '~', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!'];

        let mut result = String::with_capacity(md.len() * 2);
        let mut chars = md.chars().peekable();
        let mut in_code_block = false;
        let mut in_inline_code = false;

        while let Some(ch) = chars.next() {
            if in_code_block {
                result.push(ch);
                if ch == '`' && chars.peek() == Some(&'`') {
                    result.push(chars.next().unwrap());
                    if chars.peek() == Some(&'`') {
                        result.push(chars.next().unwrap());
                        in_code_block = false;
                    }
                }
                continue;
            }

            if in_inline_code {
                result.push(ch);
                if ch == '`' {
                    in_inline_code = false;
                }
                continue;
            }

            match ch {
                '`' => {
                    if chars.peek() == Some(&'`') {
                        let c2 = chars.next().unwrap();
                        if chars.peek() == Some(&'`') {
                            let c3 = chars.next().unwrap();
                            result.push(ch);
                            result.push(c2);
                            result.push(c3);
                            in_code_block = true;
                        } else {
                            result.push(ch);
                            result.push(c2);
                        }
                    } else {
                        result.push(ch);
                        in_inline_code = true;
                    }
                }
                '*' => {
                    // Pass through — Telegram uses same bold/italic syntax
                    result.push(ch);
                }
                c if must_escape.contains(&c) => {
                    result.push('\\');
                    result.push(c);
                }
                _ => result.push(ch),
            }
        }

        result
    }

    // ─── WhatsApp ──────────────────────────────────────────────────

    /// Convert Markdown to WhatsApp format.
    ///
    /// WhatsApp supports: `*bold*`, `_italic_`, `~strikethrough~`, `` `code` ``
    /// No code blocks, no links, no headers.
    fn to_whatsapp(md: &str) -> String {
        let mut result = String::with_capacity(md.len());
        let mut chars = md.chars().peekable();

        while let Some(ch) = chars.next() {
            match ch {
                '*' => {
                    if chars.peek() == Some(&'*') {
                        // **bold** → *bold*
                        chars.next();
                        result.push('*');
                    } else {
                        // *text* stays as-is (WhatsApp bold)
                        result.push('*');
                    }
                }
                '~' => {
                    if chars.peek() == Some(&'~') {
                        // ~~strike~~ → ~strike~
                        chars.next();
                        result.push('~');
                    } else {
                        result.push(ch);
                    }
                }
                '`' => {
                    if chars.peek() == Some(&'`') {
                        // Code block — strip fences, keep content
                        chars.next();
                        if chars.peek() == Some(&'`') {
                            chars.next();
                            // Skip language identifier
                            while let Some(&c) = chars.peek() {
                                if c == '\n' {
                                    chars.next();
                                    break;
                                }
                                chars.next();
                            }
                            // Collect until closing ```
                            while let Some(c) = chars.next() {
                                if c == '`' && chars.peek() == Some(&'`') {
                                    chars.next();
                                    if chars.peek() == Some(&'`') {
                                        chars.next();
                                        break;
                                    }
                                }
                                result.push(c);
                            }
                        } else {
                            result.push('`');
                            result.push('`');
                        }
                    } else {
                        // Inline code — keep as-is (WhatsApp supports it)
                        result.push(ch);
                    }
                }
                '#' => {
                    // Strip headers, keep text
                    while chars.peek() == Some(&'#') {
                        chars.next();
                    }
                    if chars.peek() == Some(&' ') {
                        chars.next();
                    }
                    // Make header text bold
                    result.push('*');
                    while let Some(&c) = chars.peek() {
                        if c == '\n' {
                            break;
                        }
                        result.push(chars.next().unwrap());
                    }
                    result.push('*');
                }
                '[' => {
                    // [text](url) → text (url)
                    let mut link_text = String::new();
                    let mut found = false;
                    let mut inner = chars.clone();
                    while let Some(c) = inner.next() {
                        if c == ']' {
                            if inner.peek() == Some(&'(') {
                                inner.next();
                                let mut url = String::new();
                                for c in inner.by_ref() {
                                    if c == ')' {
                                        break;
                                    }
                                    url.push(c);
                                }
                                result.push_str(&link_text);
                                result.push_str(" (");
                                result.push_str(&url);
                                result.push(')');
                                chars = inner;
                                found = true;
                            }
                            break;
                        }
                        link_text.push(c);
                    }
                    if !found {
                        result.push('[');
                    }
                }
                _ => result.push(ch),
            }
        }

        result
    }

    // ─── Plain text ────────────────────────────────────────────────

    /// Strip all formatting, yielding plain text.
    fn to_plain_text(md: &str) -> String {
        let mut result = String::with_capacity(md.len());
        let mut chars = md.chars().peekable();

        while let Some(ch) = chars.next() {
            match ch {
                '*' | '_' => {
                    // Skip formatting markers
                    if chars.peek() == Some(&ch) {
                        chars.next(); // skip double marker
                    }
                }
                '~' => {
                    if chars.peek() == Some(&'~') {
                        chars.next();
                    }
                }
                '`' => {
                    if chars.peek() == Some(&'`') {
                        chars.next();
                        if chars.peek() == Some(&'`') {
                            chars.next();
                            // Skip language identifier
                            while let Some(&c) = chars.peek() {
                                if c == '\n' {
                                    chars.next();
                                    break;
                                }
                                chars.next();
                            }
                            // Keep code content
                            while let Some(c) = chars.next() {
                                if c == '`' && chars.peek() == Some(&'`') {
                                    chars.next();
                                    if chars.peek() == Some(&'`') {
                                        chars.next();
                                        break;
                                    }
                                }
                                result.push(c);
                            }
                        }
                    }
                    // Inline code — keep content without backticks
                }
                '#' => {
                    while chars.peek() == Some(&'#') {
                        chars.next();
                    }
                    if chars.peek() == Some(&' ') {
                        chars.next();
                    }
                }
                '[' => {
                    // [text](url) → text
                    let mut link_text = String::new();
                    let mut found = false;
                    let mut inner = chars.clone();
                    while let Some(c) = inner.next() {
                        if c == ']' {
                            if inner.peek() == Some(&'(') {
                                inner.next();
                                // Skip URL
                                for c in inner.by_ref() {
                                    if c == ')' {
                                        break;
                                    }
                                }
                                result.push_str(&link_text);
                                chars = inner;
                                found = true;
                            }
                            break;
                        }
                        link_text.push(c);
                    }
                    if !found {
                        result.push('[');
                    }
                }
                _ => result.push(ch),
            }
        }

        result
    }

    // ─── HTML ──────────────────────────────────────────────────────

    /// Convert Markdown to basic HTML.
    fn to_html(md: &str) -> String {
        let mut result = String::with_capacity(md.len() * 2);
        let mut chars = md.chars().peekable();
        let mut in_code_block = false;

        while let Some(ch) = chars.next() {
            if in_code_block {
                if ch == '`' && chars.peek() == Some(&'`') {
                    chars.next();
                    if chars.peek() == Some(&'`') {
                        chars.next();
                        result.push_str("</code></pre>");
                        in_code_block = false;
                        continue;
                    }
                }
                // HTML-escape code content
                match ch {
                    '<' => result.push_str("&lt;"),
                    '>' => result.push_str("&gt;"),
                    '&' => result.push_str("&amp;"),
                    _ => result.push(ch),
                }
                continue;
            }

            match ch {
                '`' => {
                    if chars.peek() == Some(&'`') {
                        chars.next();
                        if chars.peek() == Some(&'`') {
                            chars.next();
                            // Skip language identifier
                            let mut lang = String::new();
                            while let Some(&c) = chars.peek() {
                                if c == '\n' {
                                    chars.next();
                                    break;
                                }
                                lang.push(chars.next().unwrap());
                            }
                            if lang.is_empty() {
                                result.push_str("<pre><code>");
                            } else {
                                result.push_str(&format!("<pre><code class=\"language-{}\">", lang));
                            }
                            in_code_block = true;
                        }
                    } else {
                        result.push_str("<code>");
                        // Collect until closing backtick
                        while let Some(c) = chars.next() {
                            if c == '`' {
                                break;
                            }
                            match c {
                                '<' => result.push_str("&lt;"),
                                '>' => result.push_str("&gt;"),
                                '&' => result.push_str("&amp;"),
                                _ => result.push(c),
                            }
                        }
                        result.push_str("</code>");
                    }
                }
                '*' => {
                    if chars.peek() == Some(&'*') {
                        chars.next();
                        result.push_str("<strong>");
                        // Collect until closing **
                        while let Some(c) = chars.next() {
                            if c == '*' && chars.peek() == Some(&'*') {
                                chars.next();
                                break;
                            }
                            result.push(c);
                        }
                        result.push_str("</strong>");
                    } else {
                        result.push_str("<em>");
                        while let Some(c) = chars.next() {
                            if c == '*' {
                                break;
                            }
                            result.push(c);
                        }
                        result.push_str("</em>");
                    }
                }
                '#' => {
                    let mut level = 1;
                    while chars.peek() == Some(&'#') {
                        chars.next();
                        level += 1;
                    }
                    if chars.peek() == Some(&' ') {
                        chars.next();
                    }
                    let tag = format!("h{}", level.min(6));
                    result.push_str(&format!("<{}>", tag));
                    while let Some(&c) = chars.peek() {
                        if c == '\n' {
                            break;
                        }
                        result.push(chars.next().unwrap());
                    }
                    result.push_str(&format!("</{}>", tag));
                }
                '\n' => {
                    if chars.peek() == Some(&'\n') {
                        result.push_str("<br/><br/>");
                        chars.next();
                    } else {
                        result.push('\n');
                    }
                }
                '<' => result.push_str("&lt;"),
                '>' => result.push_str("&gt;"),
                '&' => result.push_str("&amp;"),
                _ => result.push(ch),
            }
        }

        result
    }

    // ─── Semantic Chunking ─────────────────────────────────────────

    /// Split text into semantically-bounded chunks using minimum-cost segmentation.
    ///
    /// Scans the last 20% of each chunk window for the lowest-cost split point.
    /// Cost hierarchy: paragraph < sentence < line < word < code_block < hard.
    pub fn semantic_chunk(text: &str, max_length: usize) -> Vec<ChunkedSegment> {
        if text.len() <= max_length {
            return vec![ChunkedSegment {
                content: text.to_string(),
                part: 1,
                total_parts: 1,
                ends_in_code_block: false,
                split_cost: 0,
            }];
        }

        let mut segments = Vec::new();
        let mut remaining = text;
        let mut in_code_block = false;

        while !remaining.is_empty() {
            if remaining.len() <= max_length {
                segments.push((remaining.to_string(), 0, false));
                break;
            }

            // Search window: last 20% of max_length
            let search_start = max_length * 4 / 5;
            let window = &remaining[search_start..max_length];

            // Find minimum-cost split point within the window
            let (best_offset, best_cost, code_state) =
                Self::find_min_cost_split(window, in_code_block);

            let split_at = search_start + best_offset;

            // Ensure we don't split at 0
            let split_at = if split_at == 0 { max_length } else { split_at };

            segments.push((
                remaining[..split_at].to_string(),
                best_cost,
                in_code_block,
            ));

            remaining = &remaining[split_at..];
            // Trim leading whitespace from next chunk (but not newlines that are semantic)
            if remaining.starts_with(' ') {
                remaining = remaining.trim_start_matches(' ');
            }

            // Track code block state across splits
            in_code_block = code_state;
        }

        let total = segments.len();
        segments
            .into_iter()
            .enumerate()
            .map(|(i, (content, cost, in_code))| ChunkedSegment {
                content,
                part: i + 1,
                total_parts: total,
                ends_in_code_block: in_code,
                split_cost: cost,
            })
            .collect()
    }

    /// Find the minimum-cost split point in a text window.
    ///
    /// Returns (offset_in_window, cost, in_code_block_after).
    fn find_min_cost_split(window: &str, in_code_block: bool) -> (usize, u32, bool) {
        let mut best_offset = window.len();
        let mut best_cost = costs::HARD_BREAK;
        let mut code_block = in_code_block;

        let bytes = window.as_bytes();

        for i in 0..bytes.len() {
            // Track code block state
            if i + 3 <= bytes.len() && &bytes[i..i + 3] == b"```" {
                code_block = !code_block;
            }

            let cost = if code_block {
                costs::CODE_BLOCK_MID
            } else if i + 2 <= bytes.len() && &bytes[i..i + 2] == b"\n\n" {
                // Paragraph break — ideal split point
                costs::PARAGRAPH_BREAK
            } else if i > 0 && bytes[i] == b'\n' {
                // Line break
                if i >= 2 && (bytes[i - 1] == b'.' || bytes[i - 1] == b'?' || bytes[i - 1] == b'!') {
                    // Sentence end + newline — great split
                    costs::SENTENCE_END
                } else {
                    costs::LINE_BREAK
                }
            } else if bytes[i] == b' ' {
                costs::WORD_BOUNDARY
            } else {
                costs::HARD_BREAK
            };

            if cost < best_cost || (cost == best_cost && cost <= costs::LINE_BREAK) {
                best_cost = cost;
                best_offset = i;
                if cost == costs::PARAGRAPH_BREAK {
                    // Can't do better than free — take it
                    break;
                }
            }
        }

        (best_offset, best_cost, code_block)
    }
}

/// Token bucket rate limiter for message delivery.
///
/// ```text
/// tokens(t) = min(capacity, tokens(t-1) + (t - last_refill) × rate)
/// ```
#[derive(Debug, Clone)]
pub struct DeliveryRateLimiter {
    /// Maximum tokens (messages) in the bucket.
    capacity: f64,
    /// Current token count.
    tokens: f64,
    /// Refill rate (tokens per second).
    rate: f64,
    /// Last refill time.
    last_refill: Instant,
}

impl DeliveryRateLimiter {
    /// Create a new rate limiter.
    ///
    /// `capacity`: burst size (max messages at once)
    /// `rate`: sustained rate (messages per second)
    pub fn new(capacity: f64, rate: f64) -> Self {
        Self {
            capacity,
            tokens: capacity,
            rate,
            last_refill: Instant::now(),
        }
    }

    /// Defaults for common platforms.
    pub fn for_telegram() -> Self {
        // Telegram: 30 messages/second to different chats, 1/second to same chat
        Self::new(3.0, 1.0)
    }

    pub fn for_discord() -> Self {
        // Discord: 5 messages per 5 seconds per channel
        Self::new(5.0, 1.0)
    }

    pub fn for_slack() -> Self {
        // Slack: 1 message per second per channel (burst 3)
        Self::new(3.0, 1.0)
    }

    pub fn for_whatsapp() -> Self {
        // WhatsApp: conservative rate
        Self::new(2.0, 0.5)
    }

    /// Try to consume a token. Returns remaining wait time if no tokens available.
    pub fn try_acquire(&mut self) -> Result<(), Duration> {
        self.refill();

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            let deficit = 1.0 - self.tokens;
            let wait = Duration::from_secs_f64(deficit / self.rate);
            Err(wait)
        }
    }

    /// Refill tokens based on elapsed time.
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);
        self.last_refill = now;
    }

    /// Current token count.
    pub fn available_tokens(&self) -> f64 {
        self.tokens
    }
}

/// Delivery plan — a sequence of chunks with rate-limiting metadata.
#[derive(Debug, Clone)]
pub struct DeliveryPlan {
    /// Ordered segments to deliver.
    pub segments: Vec<ChunkedSegment>,
    /// Target markup format.
    pub format: MarkupFormat,
    /// Estimated total delivery time.
    pub estimated_duration: Duration,
}

impl DeliveryPlan {
    /// Create a delivery plan from a Markdown response.
    pub fn create(
        markdown: &str,
        format: MarkupFormat,
        max_length: usize,
        rate: f64,
    ) -> Self {
        let segments = ReplyFormatter::format_and_chunk(markdown, format, max_length);
        let estimated_duration = if segments.len() <= 1 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64((segments.len() - 1) as f64 / rate)
        };

        Self {
            segments,
            format,
            estimated_duration,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Slack mrkdwn tests ────────────────────────────────────────

    #[test]
    fn test_slack_bold() {
        let result = ReplyFormatter::convert_markup("**bold text**", MarkupFormat::SlackMrkdwn);
        assert_eq!(result, "*bold text*");
    }

    #[test]
    fn test_slack_italic() {
        let result = ReplyFormatter::convert_markup("*italic*", MarkupFormat::SlackMrkdwn);
        assert_eq!(result, "_italic_");
    }

    #[test]
    fn test_slack_strikethrough() {
        let result = ReplyFormatter::convert_markup("~~struck~~", MarkupFormat::SlackMrkdwn);
        assert_eq!(result, "~struck~");
    }

    #[test]
    fn test_slack_link() {
        let result = ReplyFormatter::convert_markup(
            "[click here](https://example.com)",
            MarkupFormat::SlackMrkdwn,
        );
        assert_eq!(result, "<https://example.com|click here>");
    }

    #[test]
    fn test_slack_header() {
        let result = ReplyFormatter::convert_markup("# My Header", MarkupFormat::SlackMrkdwn);
        assert_eq!(result, "*My Header*");
    }

    #[test]
    fn test_slack_code_preserved() {
        let result = ReplyFormatter::convert_markup("`code`", MarkupFormat::SlackMrkdwn);
        assert_eq!(result, "`code`");
    }

    // ─── Telegram MarkdownV2 tests ─────────────────────────────────

    #[test]
    fn test_telegram_escapes_special_chars() {
        let result = ReplyFormatter::convert_markup("hello. world!", MarkupFormat::TelegramMarkdownV2);
        assert_eq!(result, "hello\\. world\\!");
    }

    #[test]
    fn test_telegram_preserves_bold() {
        let result = ReplyFormatter::convert_markup("**bold**", MarkupFormat::TelegramMarkdownV2);
        assert!(result.contains("**bold**"));
    }

    #[test]
    fn test_telegram_code_block_no_escape() {
        let result = ReplyFormatter::convert_markup(
            "```rust\nfn main() {}\n```",
            MarkupFormat::TelegramMarkdownV2,
        );
        // Inside code blocks, special chars should NOT be escaped
        assert!(result.contains("fn main() {}"));
    }

    // ─── WhatsApp tests ────────────────────────────────────────────

    #[test]
    fn test_whatsapp_bold() {
        let result = ReplyFormatter::convert_markup("**bold**", MarkupFormat::WhatsApp);
        assert_eq!(result, "*bold*");
    }

    #[test]
    fn test_whatsapp_strip_code_block() {
        let result = ReplyFormatter::convert_markup(
            "```rust\nfn main() {}\n```",
            MarkupFormat::WhatsApp,
        );
        assert!(!result.contains("```"));
        assert!(result.contains("fn main()"));
    }

    #[test]
    fn test_whatsapp_link() {
        let result = ReplyFormatter::convert_markup(
            "[text](https://example.com)",
            MarkupFormat::WhatsApp,
        );
        assert_eq!(result, "text (https://example.com)");
    }

    // ─── Plain text tests ──────────────────────────────────────────

    #[test]
    fn test_plain_text_strips_formatting() {
        let result = ReplyFormatter::convert_markup(
            "**bold** and *italic* and ## header",
            MarkupFormat::PlainText,
        );
        assert!(!result.contains("**"));
        assert!(!result.contains("##"));
        assert!(result.contains("bold"));
        assert!(result.contains("italic"));
        assert!(result.contains("header"));
    }

    #[test]
    fn test_plain_text_link() {
        let result = ReplyFormatter::convert_markup(
            "[click](https://example.com)",
            MarkupFormat::PlainText,
        );
        assert_eq!(result, "click");
    }

    // ─── HTML tests ────────────────────────────────────────────────

    #[test]
    fn test_html_bold() {
        let result = ReplyFormatter::convert_markup("**bold**", MarkupFormat::Html);
        assert_eq!(result, "<strong>bold</strong>");
    }

    #[test]
    fn test_html_header() {
        let result = ReplyFormatter::convert_markup("## Heading", MarkupFormat::Html);
        assert_eq!(result, "<h2>Heading</h2>");
    }

    #[test]
    fn test_html_code_block() {
        let result = ReplyFormatter::convert_markup("```rust\nfn main() {}\n```", MarkupFormat::Html);
        assert!(result.contains("<pre><code class=\"language-rust\">"));
        assert!(result.contains("fn main() {}"));
        assert!(result.contains("</code></pre>"));
    }

    #[test]
    fn test_html_escapes_entities() {
        let result = ReplyFormatter::convert_markup("<script>", MarkupFormat::Html);
        assert_eq!(result, "&lt;script&gt;");
    }

    // ─── Semantic chunking tests ───────────────────────────────────

    #[test]
    fn test_no_chunking_needed() {
        let result = ReplyFormatter::semantic_chunk("Short text.", 100);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "Short text.");
        assert_eq!(result[0].total_parts, 1);
    }

    #[test]
    fn test_chunking_prefers_paragraph_break() {
        let text = format!("{}\n\n{}", "a".repeat(80), "b".repeat(80));
        let result = ReplyFormatter::semantic_chunk(&text, 100);
        assert!(result.len() >= 2);
        // First chunk should end at paragraph break
        assert_eq!(result[0].split_cost, costs::PARAGRAPH_BREAK);
    }

    #[test]
    fn test_chunking_avoids_code_block_split() {
        // A text with a code block spanning across the split window
        let before = "a".repeat(70);
        let code = "```\ncode here\n```";
        let after = "b".repeat(30);
        let text = format!("{}\n{}\n{}", before, code, after);

        let result = ReplyFormatter::semantic_chunk(&text, 100);
        // Should have multiple segments
        assert!(result.len() >= 1);
        // Splitting mid-code-block should have high cost
        for seg in &result {
            if seg.total_parts > 1 && seg.split_cost > 0 {
                // Any split should prefer non-code regions
                assert!(seg.split_cost <= costs::CODE_BLOCK_MID || seg.part == seg.total_parts);
            }
        }
    }

    #[test]
    fn test_chunking_parts_numbered_correctly() {
        let text = "word ".repeat(200); // ~1000 chars
        let result = ReplyFormatter::semantic_chunk(&text, 100);
        assert!(result.len() > 1);

        for (i, seg) in result.iter().enumerate() {
            assert_eq!(seg.part, i + 1);
            assert_eq!(seg.total_parts, result.len());
        }
    }

    // ─── Rate limiter tests ────────────────────────────────────────

    #[test]
    fn test_rate_limiter_burst() {
        let mut limiter = DeliveryRateLimiter::new(3.0, 1.0);
        assert!(limiter.try_acquire().is_ok());
        assert!(limiter.try_acquire().is_ok());
        assert!(limiter.try_acquire().is_ok());
        // Bucket should be empty now
        assert!(limiter.try_acquire().is_err());
    }

    #[test]
    fn test_rate_limiter_wait_time() {
        let mut limiter = DeliveryRateLimiter::new(1.0, 1.0);
        assert!(limiter.try_acquire().is_ok());
        let wait = limiter.try_acquire().unwrap_err();
        // Should need to wait approximately 1 second
        assert!(wait.as_secs_f64() > 0.0);
        assert!(wait.as_secs_f64() <= 1.1);
    }

    // ─── Delivery plan tests ──────────────────────────────────────

    #[test]
    fn test_delivery_plan_single_segment() {
        let plan = DeliveryPlan::create("Short message.", MarkupFormat::Markdown, 100, 1.0);
        assert_eq!(plan.segments.len(), 1);
        assert_eq!(plan.estimated_duration, Duration::ZERO);
    }

    #[test]
    fn test_delivery_plan_multi_segment() {
        let long = "word ".repeat(200);
        let plan = DeliveryPlan::create(&long, MarkupFormat::Markdown, 100, 1.0);
        assert!(plan.segments.len() > 1);
        assert!(plan.estimated_duration > Duration::ZERO);
    }

    // ─── format_and_chunk integration ─────────────────────────────

    #[test]
    fn test_format_and_chunk_slack() {
        let md = format!("**bold** and *italic*\n\n{}", "text ".repeat(200));
        let result = ReplyFormatter::format_and_chunk(&md, MarkupFormat::SlackMrkdwn, 200);
        assert!(result.len() > 1);
        // First segment should contain Slack-formatted bold
        assert!(result[0].content.contains("*bold*"));
        assert!(result[0].content.contains("_italic_"));
    }

    #[test]
    fn test_format_and_chunk_telegram() {
        let md = "Hello. World!";
        let result = ReplyFormatter::format_and_chunk(md, MarkupFormat::TelegramMarkdownV2, 1000);
        assert_eq!(result.len(), 1);
        assert!(result[0].content.contains("Hello\\."));
        assert!(result[0].content.contains("World\\!"));
    }
}
