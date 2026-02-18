//! Channel-aware Markdown rendering.
//!
//! Each messaging platform has its own text formatting conventions:
//! - **Slack**: `*bold*`, `_italic_`, `~strike~`, `` `code` ``, `> quote`
//! - **Discord**: `**bold**`, `*italic*`, `~~strike~~`, `` `code` ``, `> quote`
//! - **Telegram**: `**bold**`, `__italic__`, `~~strike~~`, `` `code` ``, HTML also supported
//! - **WhatsApp**: `*bold*`, `_italic_`, `~strike~`, `` ```code``` ``
//! - **Matrix**: full CommonMark with HTML subset
//! - **MsTeams**: subset of Markdown with HTML
//! - **IRC**: mIRC color codes + bold (Ctrl-B) / italic (Ctrl-I) / underline (Ctrl-U)
//! - **Email**: full HTML
//! - **Plain**: strip all formatting
//!
//! This module converts a common Markdown-like input into the appropriate format.

use serde::{Deserialize, Serialize};
use clawdesk_types::channel::ChannelId;

/// The target format for rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderTarget {
    Slack,
    Discord,
    Telegram,
    WhatsApp,
    Matrix,
    MsTeams,
    Irc,
    Email,
    Line,
    GoogleChat,
    Nostr,
    Signal,
    IMessage,
    Mattermost,
    Plain,
}

impl From<ChannelId> for RenderTarget {
    fn from(ch: ChannelId) -> Self {
        match ch {
            ChannelId::Slack => RenderTarget::Slack,
            ChannelId::Discord => RenderTarget::Discord,
            ChannelId::Telegram => RenderTarget::Telegram,
            ChannelId::WhatsApp => RenderTarget::WhatsApp,
            ChannelId::Matrix => RenderTarget::Matrix,
            ChannelId::MsTeams => RenderTarget::MsTeams,
            ChannelId::Irc => RenderTarget::Irc,
            ChannelId::Email => RenderTarget::Email,
            ChannelId::Line => RenderTarget::Line,
            ChannelId::GoogleChat => RenderTarget::GoogleChat,
            ChannelId::Nostr => RenderTarget::Nostr,
            ChannelId::Signal => RenderTarget::Signal,
            ChannelId::IMessage => RenderTarget::IMessage,
            ChannelId::Mattermost => RenderTarget::Mattermost,
            _ => RenderTarget::Plain,
        }
    }
}

/// Inline formatting span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    Text(String),
    Bold(Vec<Span>),
    Italic(Vec<Span>),
    Strike(Vec<Span>),
    Code(String),
    Link { text: String, url: String },
}

/// Block-level element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Span>),
    CodeBlock { language: Option<String>, code: String },
    Quote(Vec<Block>),
    UnorderedList(Vec<Vec<Span>>),
    OrderedList(Vec<Vec<Span>>),
    Heading { level: u8, content: Vec<Span> },
    HorizontalRule,
}

/// A parsed document ready for rendering.
#[derive(Debug, Clone)]
pub struct Document {
    pub blocks: Vec<Block>,
}

/// Render a common markdown string into channel-specific format.
///
/// This is the primary entry point. It parses a subset of CommonMark, then
/// renders into the target format.
pub fn render(input: &str, target: RenderTarget) -> String {
    let doc = parse(input);
    render_document(&doc, target)
}

// ── Parser ────────────────────────────────────────────────

/// Simple markdown parser. Handles:
/// - `**bold**` / `__bold__`
/// - `*italic*` / `_italic_`
/// - `~~strike~~`
/// - `` `code` ``
/// - `[text](url)` links
/// - ``` ```lang\ncode``` ``` code blocks
/// - `> quotes`
/// - `# headings` (1-3 levels)
/// - `- unordered` and `1. ordered` lists
/// - `---` horizontal rules
fn parse(input: &str) -> Document {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = input.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        // Code block
        if line.trim_start().starts_with("```") {
            let indent = line.len() - line.trim_start().len();
            let lang_part = line.trim_start().strip_prefix("```").unwrap().trim();
            let language = if lang_part.is_empty() {
                None
            } else {
                Some(lang_part.to_string())
            };
            let mut code_lines = Vec::new();
            i += 1;
            while i < lines.len() {
                let cl = lines[i];
                if cl.trim_start().starts_with("```") {
                    i += 1;
                    break;
                }
                // Strip common indent
                let stripped = if cl.len() > indent {
                    &cl[indent.min(cl.len())..]
                } else {
                    cl
                };
                code_lines.push(stripped);
                i += 1;
            }
            blocks.push(Block::CodeBlock {
                language,
                code: code_lines.join("\n"),
            });
            continue;
        }

        // Horizontal rule
        if line.trim() == "---" || line.trim() == "***" || line.trim() == "___" {
            blocks.push(Block::HorizontalRule);
            i += 1;
            continue;
        }

        // Heading
        if let Some(rest) = line.strip_prefix("### ") {
            blocks.push(Block::Heading {
                level: 3,
                content: parse_spans(rest),
            });
            i += 1;
            continue;
        }
        if let Some(rest) = line.strip_prefix("## ") {
            blocks.push(Block::Heading {
                level: 2,
                content: parse_spans(rest),
            });
            i += 1;
            continue;
        }
        if let Some(rest) = line.strip_prefix("# ") {
            blocks.push(Block::Heading {
                level: 1,
                content: parse_spans(rest),
            });
            i += 1;
            continue;
        }

        // Quote
        if line.starts_with("> ") || line == ">" {
            let mut quote_lines = Vec::new();
            while i < lines.len()
                && (lines[i].starts_with("> ") || lines[i] == ">")
            {
                let ql = lines[i].strip_prefix("> ").unwrap_or("");
                quote_lines.push(ql);
                i += 1;
            }
            let inner = quote_lines.join("\n");
            let inner_doc = parse(&inner);
            blocks.push(Block::Quote(inner_doc.blocks));
            continue;
        }

        // Unordered list
        if line.starts_with("- ") || line.starts_with("* ") {
            let mut items = Vec::new();
            while i < lines.len()
                && (lines[i].starts_with("- ") || lines[i].starts_with("* "))
            {
                let item_text = &lines[i][2..];
                items.push(parse_spans(item_text));
                i += 1;
            }
            blocks.push(Block::UnorderedList(items));
            continue;
        }

        // Ordered list
        if line.len() > 2 && line.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            if let Some(rest) = try_strip_ordered_prefix(line) {
                let mut items = Vec::new();
                items.push(parse_spans(rest));
                i += 1;
                while i < lines.len() {
                    if let Some(r) = try_strip_ordered_prefix(lines[i]) {
                        items.push(parse_spans(r));
                        i += 1;
                    } else {
                        break;
                    }
                }
                blocks.push(Block::OrderedList(items));
                continue;
            }
        }

        // Empty line — skip
        if line.trim().is_empty() {
            i += 1;
            continue;
        }

        // Paragraph — collect contiguous non-empty, non-special lines
        let mut para_text = String::from(line);
        i += 1;
        while i < lines.len() {
            let next = lines[i];
            if next.trim().is_empty()
                || next.starts_with("# ")
                || next.starts_with("## ")
                || next.starts_with("### ")
                || next.starts_with("> ")
                || next.starts_with("- ")
                || next.starts_with("* ")
                || next.trim_start().starts_with("```")
                || next.trim() == "---"
            {
                break;
            }
            para_text.push(' ');
            para_text.push_str(next);
            i += 1;
        }
        blocks.push(Block::Paragraph(parse_spans(&para_text)));
    }

    Document { blocks }
}

fn try_strip_ordered_prefix(s: &str) -> Option<&str> {
    let trimmed = s.trim_start();
    let dot_pos = trimmed.find(". ")?;
    let num_part = &trimmed[..dot_pos];
    if num_part.chars().all(|c| c.is_ascii_digit()) && !num_part.is_empty() {
        Some(&trimmed[dot_pos + 2..])
    } else {
        None
    }
}

/// Parse inline spans from text.
fn parse_spans(input: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut chars: Vec<char> = input.chars().collect();
    let mut pos = 0;
    let mut buf = String::new();

    while pos < chars.len() {
        // Inline code
        if chars[pos] == '`' {
            if !buf.is_empty() {
                spans.push(Span::Text(std::mem::take(&mut buf)));
            }
            pos += 1;
            let mut code = String::new();
            while pos < chars.len() && chars[pos] != '`' {
                code.push(chars[pos]);
                pos += 1;
            }
            if pos < chars.len() {
                pos += 1; // skip closing `
            }
            spans.push(Span::Code(code));
            continue;
        }

        // Bold: **text**
        if pos + 1 < chars.len() && chars[pos] == '*' && chars[pos + 1] == '*' {
            if !buf.is_empty() {
                spans.push(Span::Text(std::mem::take(&mut buf)));
            }
            pos += 2;
            let mut inner = String::new();
            while pos + 1 < chars.len() && !(chars[pos] == '*' && chars[pos + 1] == '*') {
                inner.push(chars[pos]);
                pos += 1;
            }
            if pos + 1 < chars.len() {
                pos += 2;
            }
            spans.push(Span::Bold(parse_spans(&inner)));
            continue;
        }

        // Strikethrough: ~~text~~
        if pos + 1 < chars.len() && chars[pos] == '~' && chars[pos + 1] == '~' {
            if !buf.is_empty() {
                spans.push(Span::Text(std::mem::take(&mut buf)));
            }
            pos += 2;
            let mut inner = String::new();
            while pos + 1 < chars.len() && !(chars[pos] == '~' && chars[pos + 1] == '~') {
                inner.push(chars[pos]);
                pos += 1;
            }
            if pos + 1 < chars.len() {
                pos += 2;
            }
            spans.push(Span::Strike(parse_spans(&inner)));
            continue;
        }

        // Italic: *text* (single asterisk, not at word boundary issues — simplified)
        if chars[pos] == '*' {
            if !buf.is_empty() {
                spans.push(Span::Text(std::mem::take(&mut buf)));
            }
            pos += 1;
            let mut inner = String::new();
            while pos < chars.len() && chars[pos] != '*' {
                inner.push(chars[pos]);
                pos += 1;
            }
            if pos < chars.len() {
                pos += 1;
            }
            spans.push(Span::Italic(parse_spans(&inner)));
            continue;
        }

        // Link: [text](url)
        if chars[pos] == '[' {
            if !buf.is_empty() {
                spans.push(Span::Text(std::mem::take(&mut buf)));
            }
            let start = pos;
            pos += 1;
            let mut text = String::new();
            while pos < chars.len() && chars[pos] != ']' {
                text.push(chars[pos]);
                pos += 1;
            }
            if pos < chars.len() && pos + 1 < chars.len() && chars[pos] == ']' && chars[pos + 1] == '(' {
                pos += 2;
                let mut url = String::new();
                while pos < chars.len() && chars[pos] != ')' {
                    url.push(chars[pos]);
                    pos += 1;
                }
                if pos < chars.len() {
                    pos += 1;
                }
                spans.push(Span::Link { text, url });
            } else {
                // Not a valid link — treat as text
                buf.push('[');
                buf.push_str(&text);
                if pos < chars.len() {
                    buf.push(chars[pos]);
                    pos += 1;
                }
            }
            continue;
        }

        buf.push(chars[pos]);
        pos += 1;
    }

    if !buf.is_empty() {
        spans.push(Span::Text(buf));
    }

    spans
}

// ── Renderers ─────────────────────────────────────────────

fn render_document(doc: &Document, target: RenderTarget) -> String {
    let mut parts: Vec<String> = Vec::new();
    for block in &doc.blocks {
        parts.push(render_block(block, target));
    }
    parts.join("\n\n")
}

fn render_block(block: &Block, target: RenderTarget) -> String {
    match block {
        Block::Paragraph(spans) => render_spans(spans, target),
        Block::CodeBlock { language, code } => render_code_block(language.as_deref(), code, target),
        Block::Quote(blocks) => render_quote(blocks, target),
        Block::UnorderedList(items) => render_unordered(items, target),
        Block::OrderedList(items) => render_ordered(items, target),
        Block::Heading { level, content } => render_heading(*level, content, target),
        Block::HorizontalRule => render_hr(target),
    }
}

fn render_spans(spans: &[Span], target: RenderTarget) -> String {
    spans.iter().map(|s| render_span(s, target)).collect()
}

fn render_span(span: &Span, target: RenderTarget) -> String {
    match span {
        Span::Text(t) => t.clone(),
        Span::Code(c) => format!("`{}`", c),
        Span::Bold(inner) => {
            let content = render_spans(inner, target);
            match target {
                RenderTarget::Slack | RenderTarget::WhatsApp => format!("*{}*", content),
                RenderTarget::Discord | RenderTarget::Telegram | RenderTarget::Mattermost => {
                    format!("**{}**", content)
                }
                RenderTarget::Matrix | RenderTarget::Email => {
                    format!("<b>{}</b>", content)
                }
                RenderTarget::Irc => format!("\x02{}\x02", content),
                RenderTarget::Plain => content,
                _ => format!("**{}**", content),
            }
        }
        Span::Italic(inner) => {
            let content = render_spans(inner, target);
            match target {
                RenderTarget::Slack | RenderTarget::WhatsApp => format!("_{}_", content),
                RenderTarget::Discord | RenderTarget::Telegram | RenderTarget::Mattermost => {
                    format!("*{}*", content)
                }
                RenderTarget::Matrix | RenderTarget::Email => {
                    format!("<i>{}</i>", content)
                }
                RenderTarget::Irc => format!("\x1D{}\x1D", content),
                RenderTarget::Plain => content,
                _ => format!("*{}*", content),
            }
        }
        Span::Strike(inner) => {
            let content = render_spans(inner, target);
            match target {
                RenderTarget::Slack | RenderTarget::WhatsApp => format!("~{}~", content),
                RenderTarget::Discord | RenderTarget::Telegram | RenderTarget::Mattermost => {
                    format!("~~{}~~", content)
                }
                RenderTarget::Matrix | RenderTarget::Email => {
                    format!("<s>{}</s>", content)
                }
                RenderTarget::Plain => content,
                _ => format!("~~{}~~", content),
            }
        }
        Span::Link { text, url } => match target {
            RenderTarget::Slack => format!("<{}|{}>", url, text),
            RenderTarget::Discord | RenderTarget::Telegram | RenderTarget::Mattermost => {
                format!("[{}]({})", text, url)
            }
            RenderTarget::Matrix | RenderTarget::Email => {
                format!("<a href=\"{}\">{}</a>", url, text)
            }
            RenderTarget::Plain => format!("{} ({})", text, url),
            _ => format!("[{}]({})", text, url),
        },
    }
}

fn render_code_block(language: Option<&str>, code: &str, target: RenderTarget) -> String {
    match target {
        RenderTarget::Slack => {
            format!("```\n{}\n```", code)
        }
        RenderTarget::Discord
        | RenderTarget::Telegram
        | RenderTarget::Matrix
        | RenderTarget::Mattermost => {
            let lang = language.unwrap_or("");
            format!("```{}\n{}\n```", lang, code)
        }
        RenderTarget::WhatsApp => {
            format!("```\n{}\n```", code)
        }
        RenderTarget::Email => {
            format!("<pre><code>{}</code></pre>", html_escape(code))
        }
        RenderTarget::Plain => code.to_string(),
        _ => format!("```\n{}\n```", code),
    }
}

fn render_quote(blocks: &[Block], target: RenderTarget) -> String {
    let inner: Vec<String> = blocks.iter().map(|b| render_block(b, target)).collect();
    let joined = inner.join("\n");
    match target {
        RenderTarget::Email => format!("<blockquote>{}</blockquote>", joined),
        RenderTarget::Plain => {
            joined
                .lines()
                .map(|l| format!("  {}", l))
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => {
            joined
                .lines()
                .map(|l| format!("> {}", l))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

fn render_unordered(items: &[Vec<Span>], target: RenderTarget) -> String {
    match target {
        RenderTarget::Email => {
            let lis: Vec<String> = items
                .iter()
                .map(|i| format!("<li>{}</li>", render_spans(i, target)))
                .collect();
            format!("<ul>{}</ul>", lis.join(""))
        }
        _ => {
            items
                .iter()
                .map(|i| format!("• {}", render_spans(i, target)))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

fn render_ordered(items: &[Vec<Span>], target: RenderTarget) -> String {
    match target {
        RenderTarget::Email => {
            let lis: Vec<String> = items
                .iter()
                .map(|i| format!("<li>{}</li>", render_spans(i, target)))
                .collect();
            format!("<ol>{}</ol>", lis.join(""))
        }
        _ => {
            items
                .iter()
                .enumerate()
                .map(|(idx, i)| format!("{}. {}", idx + 1, render_spans(i, target)))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

fn render_heading(level: u8, content: &[Span], target: RenderTarget) -> String {
    let text = render_spans(content, target);
    match target {
        RenderTarget::Email | RenderTarget::Matrix => {
            format!("<h{}>{}</h{}>", level, text, level)
        }
        RenderTarget::Plain => {
            let prefix = match level {
                1 => "═══ ",
                2 => "─── ",
                _ => "··· ",
            };
            format!("{}{}", prefix, text)
        }
        RenderTarget::Irc => format!("\x02{}\x02", text),
        _ => {
            let hashes = "#".repeat(level as usize);
            format!("{} {}", hashes, text)
        }
    }
}

fn render_hr(target: RenderTarget) -> String {
    match target {
        RenderTarget::Email | RenderTarget::Matrix => "<hr/>".to_string(),
        RenderTarget::Plain => "────────────────".to_string(),
        _ => "---".to_string(),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bold_rendering() {
        let input = "Hello **world**!";
        assert_eq!(render(input, RenderTarget::Slack), "Hello *world*!");
        assert_eq!(render(input, RenderTarget::Discord), "Hello **world**!");
        assert_eq!(render(input, RenderTarget::Email), "Hello <b>world</b>!");
        assert_eq!(render(input, RenderTarget::Plain), "Hello world!");
    }

    #[test]
    fn test_italic_rendering() {
        let input = "Hello *world*!";
        assert_eq!(render(input, RenderTarget::Slack), "Hello _world_!");
        assert_eq!(render(input, RenderTarget::Discord), "Hello *world*!");
    }

    #[test]
    fn test_link_rendering() {
        let input = "Click [here](https://example.com)";
        assert_eq!(
            render(input, RenderTarget::Slack),
            "Click <https://example.com|here>"
        );
        assert_eq!(
            render(input, RenderTarget::Discord),
            "Click [here](https://example.com)"
        );
        assert_eq!(
            render(input, RenderTarget::Email),
            "Click <a href=\"https://example.com\">here</a>"
        );
    }

    #[test]
    fn test_code_block() {
        let input = "```rust\nfn main() {}\n```";
        assert_eq!(
            render(input, RenderTarget::Discord),
            "```rust\nfn main() {}\n```"
        );
        assert_eq!(
            render(input, RenderTarget::Email),
            "<pre><code>fn main() {}</code></pre>"
        );
    }

    #[test]
    fn test_quote() {
        let input = "> quoted text";
        assert_eq!(render(input, RenderTarget::Discord), "> quoted text");
        assert_eq!(
            render(input, RenderTarget::Email),
            "<blockquote>quoted text</blockquote>"
        );
    }

    #[test]
    fn test_heading() {
        let input = "# Title";
        assert_eq!(render(input, RenderTarget::Discord), "# Title");
        assert_eq!(render(input, RenderTarget::Email), "<h1>Title</h1>");
        assert_eq!(render(input, RenderTarget::Plain), "═══ Title");
    }

    #[test]
    fn test_strikethrough() {
        let input = "~~deleted~~";
        assert_eq!(render(input, RenderTarget::Slack), "~deleted~");
        assert_eq!(render(input, RenderTarget::Discord), "~~deleted~~");
        assert_eq!(render(input, RenderTarget::Email), "<s>deleted</s>");
    }

    #[test]
    fn test_unordered_list() {
        let input = "- one\n- two\n- three";
        let result = render(input, RenderTarget::Discord);
        assert!(result.contains("• one"));
        assert!(result.contains("• two"));
        assert!(result.contains("• three"));
    }

    #[test]
    fn test_channel_id_conversion() {
        assert_eq!(RenderTarget::from(ChannelId::Slack), RenderTarget::Slack);
        assert_eq!(
            RenderTarget::from(ChannelId::Discord),
            RenderTarget::Discord
        );
        assert_eq!(RenderTarget::from(ChannelId::Email), RenderTarget::Email);
    }

    #[test]
    fn test_irc_bold() {
        let input = "**bold**";
        let result = render(input, RenderTarget::Irc);
        assert!(result.contains("\x02bold\x02"));
    }
}
