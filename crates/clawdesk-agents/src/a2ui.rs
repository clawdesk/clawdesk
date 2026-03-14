//! # A2UI — Agent-to-UI Rendering Pipeline
//!
//! A full transport-aware rendering system that converts structured agent
//! output into native representations for every transport:
//!
//! ```text
//!                Agent Response
//!                     │
//!              ┌──────┴──────┐
//!              │  A2UI Parse │
//!              └──────┬──────┘
//!                     │
//!    ┌────────────────┼────────────────┐
//!    │                │                │
//!    ▼                ▼                ▼
//! ┌──────┐      ┌──────────┐    ┌──────────┐
//! │ Tauri│      │  CLI/TTY │    │ Channel  │
//! │ React│      │  ANSI    │    │ Adapter  │
//! └──────┘      └──────────┘    └──────────┘
//!    │                │                │
//!    ▼                ▼                ▼
//! React JSX     Colored text     Discord embed
//! components    + tables         Slack blocks
//!                                Telegram kbd
//! ```
//!
//! ## Design Principles
//!
//! 1. **Single source format** — agents produce one `A2uiResponse`
//! 2. **Transport renders natively** — each transport has a `Renderer`
//! 3. **Graceful degradation** — if a transport can't render a component,
//!    it falls back to markdown text
//! 4. **Multimodal** — supports text, images, audio, files, not just UI

use serde::{Deserialize, Serialize};

// ═══════════════════════════════════════════════════════════════════════════════
// COMPONENT MODEL
// ═══════════════════════════════════════════════════════════════════════════════

/// A complete agent response with optional structured UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2uiResponse {
    /// Plain text fallback (always present).
    pub text: String,
    /// Structured components (optional — agents can produce text-only).
    #[serde(default)]
    pub components: Vec<Component>,
    /// Attached media (images, audio, files).
    #[serde(default)]
    pub media: Vec<MediaAttachment>,
    /// Suggested user actions (buttons/quick replies).
    #[serde(default)]
    pub actions: Vec<Action>,
}

/// A renderable UI component.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Component {
    /// Data card with title, fields, and optional image.
    Card {
        id: String,
        title: String,
        #[serde(default)]
        subtitle: Option<String>,
        #[serde(default)]
        fields: Vec<Field>,
        #[serde(default)]
        image_url: Option<String>,
        #[serde(default)]
        color: Option<String>,
    },
    /// Tabular data.
    Table {
        id: String,
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
        #[serde(default)]
        caption: Option<String>,
    },
    /// Syntax-highlighted code block.
    Code {
        id: String,
        language: String,
        code: String,
        #[serde(default)]
        filename: Option<String>,
    },
    /// KPI / metric display.
    Metric {
        id: String,
        label: String,
        value: String,
        #[serde(default)]
        unit: Option<String>,
        #[serde(default)]
        trend: Option<Trend>,
        #[serde(default)]
        sparkline: Vec<f64>,
    },
    /// Alert / callout box.
    Alert {
        id: String,
        level: AlertLevel,
        title: String,
        #[serde(default)]
        body: Option<String>,
    },
    /// Progress bar.
    Progress {
        id: String,
        label: String,
        value: f64,
        max: f64,
        #[serde(default)]
        status: Option<String>,
    },
    /// Timeline / log entries.
    Timeline {
        id: String,
        entries: Vec<TimelineEntry>,
    },
    /// File tree visualization.
    FileTree {
        id: String,
        root: String,
        nodes: Vec<FileNode>,
    },
    /// Diff / patch view.
    Diff {
        id: String,
        filename: String,
        hunks: Vec<DiffHunk>,
    },
    /// Markdown section (rich text).
    Section {
        id: String,
        markdown: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    pub label: String,
    pub value: String,
    #[serde(default)]
    pub inline: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trend { Up, Down, Flat }

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertLevel { Info, Success, Warning, Error }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub timestamp: String,
    pub label: String,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileNode {
    pub name: String,
    pub is_dir: bool,
    #[serde(default)]
    pub children: Vec<FileNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffHunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub content: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffLineKind { Context, Add, Remove }

/// Media attachment (image, audio, file).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaAttachment {
    pub id: String,
    pub media_type: MediaType,
    /// Base64-encoded data OR a URL.
    pub data: String,
    pub mime: String,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub alt_text: Option<String>,
    /// Size in bytes (for display).
    #[serde(default)]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaType { Image, Audio, Video, File }

/// An interactive action the user can take.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub label: String,
    pub style: ActionStyle,
    /// What happens when clicked — transport-specific.
    pub payload: ActionPayload,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionStyle { Primary, Secondary, Danger }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ActionPayload {
    /// Send a message to the agent.
    SendMessage { text: String },
    /// Open a URL.
    OpenUrl { url: String },
    /// Copy text to clipboard.
    CopyToClipboard { text: String },
    /// Execute a tool call.
    ToolCall { tool: String, args: serde_json::Value },
}

// ═══════════════════════════════════════════════════════════════════════════════
// RENDERERS
// ═══════════════════════════════════════════════════════════════════════════════

/// Target transport for rendering.
#[derive(Debug, Clone, Copy)]
pub enum Transport {
    /// Tauri desktop (React components via IPC).
    Desktop,
    /// CLI terminal (ANSI escape codes).
    Cli,
    /// TMUX pane (constrained width).
    Tmux,
    /// Discord (embeds + buttons).
    Discord,
    /// Slack (Block Kit).
    Slack,
    /// Telegram (inline keyboards + HTML).
    Telegram,
    /// Email (HTML).
    Email,
    /// Plain text (no formatting — pipe mode, logging).
    Plain,
}

/// Render an A2UI response for a specific transport.
pub fn render(response: &A2uiResponse, transport: Transport) -> String {
    match transport {
        Transport::Cli | Transport::Tmux => render_cli(response),
        Transport::Discord => render_discord(response),
        Transport::Slack => render_slack(response),
        Transport::Telegram => render_telegram(response),
        Transport::Plain => render_plain(response),
        _ => render_markdown(response),
    }
}

// ─── CLI Renderer (ANSI) ─────────────────────────────────────────────────────

fn render_cli(resp: &A2uiResponse) -> String {
    let mut out = String::new();

    for comp in &resp.components {
        match comp {
            Component::Card { title, subtitle, fields, .. } => {
                out.push_str(&format!("\x1b[1;36m┌─ {} ─┐\x1b[0m\n", title));
                if let Some(sub) = subtitle {
                    out.push_str(&format!("  \x1b[2m{}\x1b[0m\n", sub));
                }
                for f in fields {
                    out.push_str(&format!("  \x1b[1m{}:\x1b[0m {}\n", f.label, f.value));
                }
                out.push_str("\x1b[2m└────────────┘\x1b[0m\n");
            }
            Component::Table { headers, rows, caption, .. } => {
                if let Some(cap) = caption {
                    out.push_str(&format!("\x1b[1m{}\x1b[0m\n", cap));
                }
                // Column widths
                let cols = headers.len();
                let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
                for row in rows {
                    for (i, cell) in row.iter().enumerate() {
                        if i < cols { widths[i] = widths[i].max(cell.len()); }
                    }
                }
                // Header
                let header_line: String = headers.iter().enumerate()
                    .map(|(i, h)| format!(" {:w$} ", h, w = widths[i]))
                    .collect::<Vec<_>>().join("│");
                out.push_str(&format!("\x1b[1m{}\x1b[0m\n", header_line));
                let sep: String = widths.iter().map(|w| "─".repeat(w + 2)).collect::<Vec<_>>().join("┼");
                out.push_str(&format!("{}\n", sep));
                for row in rows {
                    let line: String = row.iter().enumerate()
                        .map(|(i, c)| format!(" {:w$} ", c, w = if i < cols { widths[i] } else { c.len() }))
                        .collect::<Vec<_>>().join("│");
                    out.push_str(&format!("{}\n", line));
                }
            }
            Component::Code { language, code, filename, .. } => {
                let header = filename.as_deref().unwrap_or(language);
                out.push_str(&format!("\x1b[2m── {} ──\x1b[0m\n", header));
                for (i, line) in code.lines().enumerate() {
                    out.push_str(&format!("\x1b[2m{:4}\x1b[0m│ {}\n", i + 1, line));
                }
            }
            Component::Metric { label, value, unit, trend, .. } => {
                let arrow = match trend {
                    Some(Trend::Up) => "\x1b[32m↑\x1b[0m",
                    Some(Trend::Down) => "\x1b[31m↓\x1b[0m",
                    Some(Trend::Flat) => "→",
                    None => "",
                };
                let unit_str = unit.as_deref().unwrap_or("");
                out.push_str(&format!("  \x1b[1m{}\x1b[0m: {} {}{}\n", label, value, unit_str, arrow));
            }
            Component::Alert { level, title, body, .. } => {
                let (icon, color) = match level {
                    AlertLevel::Info => ("ℹ", "34"),
                    AlertLevel::Success => ("✓", "32"),
                    AlertLevel::Warning => ("⚠", "33"),
                    AlertLevel::Error => ("✗", "31"),
                };
                out.push_str(&format!("\x1b[{}m{} {}\x1b[0m\n", color, icon, title));
                if let Some(b) = body { out.push_str(&format!("  {}\n", b)); }
            }
            Component::Progress { label, value, max, .. } => {
                let pct = if *max > 0.0 { value / max } else { 0.0 };
                let bar_w = 30;
                let filled = (pct * bar_w as f64) as usize;
                let bar: String = format!("{}{}", "█".repeat(filled), "░".repeat(bar_w - filled));
                out.push_str(&format!("  {} [{}] {:.0}%\n", label, bar, pct * 100.0));
            }
            _ => {}
        }
    }

    // Actions as numbered options
    if !resp.actions.is_empty() {
        out.push_str("\n\x1b[1mActions:\x1b[0m\n");
        for (i, action) in resp.actions.iter().enumerate() {
            out.push_str(&format!("  \x1b[36m[{}]\x1b[0m {}\n", i + 1, action.label));
        }
    }

    if !resp.text.is_empty() && resp.components.is_empty() {
        out.push_str(&resp.text);
    }

    out
}

// ─── Discord Renderer (Embeds) ───────────────────────────────────────────────

fn render_discord(resp: &A2uiResponse) -> String {
    // Discord uses JSON embeds — we return a JSON structure the channel
    // adapter can parse and send via Discord's embed API.
    let mut embeds: Vec<serde_json::Value> = Vec::new();

    for comp in &resp.components {
        match comp {
            Component::Card { title, subtitle, fields, color, .. } => {
                let discord_fields: Vec<serde_json::Value> = fields.iter().map(|f| {
                    serde_json::json!({"name": f.label, "value": f.value, "inline": f.inline})
                }).collect();
                embeds.push(serde_json::json!({
                    "title": title,
                    "description": subtitle,
                    "color": parse_color(color.as_deref()),
                    "fields": discord_fields,
                }));
            }
            Component::Alert { level, title, body, .. } => {
                let color = match level {
                    AlertLevel::Info => 0x3498db,
                    AlertLevel::Success => 0x2ecc71,
                    AlertLevel::Warning => 0xf39c12,
                    AlertLevel::Error => 0xe74c3c,
                };
                embeds.push(serde_json::json!({
                    "title": title,
                    "description": body,
                    "color": color,
                }));
            }
            _ => {}
        }
    }

    if embeds.is_empty() {
        resp.text.clone()
    } else {
        serde_json::json!({"content": &resp.text, "embeds": embeds}).to_string()
    }
}

// ─── Slack Renderer (Block Kit) ──────────────────────────────────────────────

fn render_slack(resp: &A2uiResponse) -> String {
    let mut blocks: Vec<serde_json::Value> = Vec::new();

    if !resp.text.is_empty() {
        blocks.push(serde_json::json!({
            "type": "section",
            "text": {"type": "mrkdwn", "text": &resp.text}
        }));
    }

    for comp in &resp.components {
        match comp {
            Component::Card { title, fields, .. } => {
                blocks.push(serde_json::json!({"type": "header", "text": {"type": "plain_text", "text": title}}));
                let slack_fields: Vec<serde_json::Value> = fields.iter().map(|f| {
                    serde_json::json!({"type": "mrkdwn", "text": format!("*{}*\n{}", f.label, f.value)})
                }).collect();
                if !slack_fields.is_empty() {
                    blocks.push(serde_json::json!({"type": "section", "fields": slack_fields}));
                }
            }
            _ => {}
        }
    }

    // Actions as Slack buttons
    if !resp.actions.is_empty() {
        let elements: Vec<serde_json::Value> = resp.actions.iter().map(|a| {
            serde_json::json!({
                "type": "button",
                "text": {"type": "plain_text", "text": &a.label},
                "action_id": &a.id,
            })
        }).collect();
        blocks.push(serde_json::json!({"type": "actions", "elements": elements}));
    }

    serde_json::json!({"blocks": blocks}).to_string()
}

// ─── Telegram Renderer (HTML + Inline Keyboard) ──────────────────────────────

fn render_telegram(resp: &A2uiResponse) -> String {
    let mut html = String::new();

    for comp in &resp.components {
        match comp {
            Component::Card { title, fields, .. } => {
                html.push_str(&format!("<b>{}</b>\n", title));
                for f in fields {
                    html.push_str(&format!("<b>{}:</b> {}\n", f.label, f.value));
                }
                html.push('\n');
            }
            Component::Alert { level, title, body, .. } => {
                let emoji = match level {
                    AlertLevel::Info => "ℹ️",
                    AlertLevel::Success => "✅",
                    AlertLevel::Warning => "⚠️",
                    AlertLevel::Error => "❌",
                };
                html.push_str(&format!("{} <b>{}</b>\n", emoji, title));
                if let Some(b) = body { html.push_str(&format!("{}\n", b)); }
            }
            _ => {}
        }
    }

    if html.is_empty() { resp.text.clone() } else { html }
}

// ─── Plain Text Renderer ─────────────────────────────────────────────────────

fn render_plain(resp: &A2uiResponse) -> String {
    let mut out = resp.text.clone();
    for comp in &resp.components {
        match comp {
            Component::Table { headers, rows, .. } => {
                out.push('\n');
                out.push_str(&headers.join("\t"));
                out.push('\n');
                for row in rows { out.push_str(&row.join("\t")); out.push('\n'); }
            }
            Component::Code { code, .. } => {
                out.push_str("\n```\n");
                out.push_str(code);
                out.push_str("\n```\n");
            }
            _ => {}
        }
    }
    out
}

// ─── Markdown Renderer (Desktop/default) ─────────────────────────────────────

fn render_markdown(resp: &A2uiResponse) -> String {
    let mut out = resp.text.clone();
    for comp in &resp.components {
        match comp {
            Component::Table { headers, rows, caption, .. } => {
                out.push('\n');
                if let Some(cap) = caption { out.push_str(&format!("**{}**\n\n", cap)); }
                out.push_str(&format!("| {} |\n", headers.join(" | ")));
                out.push_str(&format!("| {} |\n", headers.iter().map(|_| "---").collect::<Vec<_>>().join(" | ")));
                for row in rows {
                    out.push_str(&format!("| {} |\n", row.join(" | ")));
                }
            }
            Component::Code { language, code, filename, .. } => {
                if let Some(f) = filename { out.push_str(&format!("\n*{}*\n", f)); }
                out.push_str(&format!("\n```{}\n{}\n```\n", language, code));
            }
            _ => {}
        }
    }
    out
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn parse_color(hex: Option<&str>) -> u32 {
    hex.and_then(|s| {
        let s = s.trim_start_matches('#');
        u32::from_str_radix(s, 16).ok()
    }).unwrap_or(0x7289da)
}

// ═══════════════════════════════════════════════════════════════════════════════
// PARSER — Extract A2UI from agent text
// ═══════════════════════════════════════════════════════════════════════════════

/// Extract A2UI blocks from agent text output.
pub fn extract_a2ui_blocks(text: &str) -> Vec<A2uiResponse> {
    let mut results = Vec::new();
    let mut remaining = text;
    while let Some(start) = remaining.find("<a2ui>") {
        let after = &remaining[start + 6..];
        if let Some(end) = after.find("</a2ui>") {
            if let Ok(resp) = serde_json::from_str::<A2uiResponse>(&after[..end].trim()) {
                results.push(resp);
            }
            remaining = &after[end + 7..];
        } else { break; }
    }
    results
}

pub fn has_a2ui_content(text: &str) -> bool {
    text.contains("<a2ui>") && text.contains("</a2ui>")
}

/// System prompt fragment that teaches agents the A2UI protocol.
pub const A2UI_SYSTEM_PROMPT: &str = r#"
<a2ui_protocol>
When your response benefits from structured display, wrap it in <a2ui> tags:

<a2ui>
{
  "text": "Brief summary for transports that can't render components.",
  "components": [
    {"type": "card", "id": "c1", "title": "...", "fields": [{"label": "...", "value": "..."}]},
    {"type": "table", "id": "t1", "headers": ["A","B"], "rows": [["1","2"]]},
    {"type": "code", "id": "x1", "language": "rust", "code": "fn main() {}"},
    {"type": "metric", "id": "m1", "label": "Score", "value": "95%", "trend": "up"},
    {"type": "alert", "id": "a1", "level": "success", "title": "Done"},
    {"type": "progress", "id": "p1", "label": "Build", "value": 8, "max": 10},
    {"type": "diff", "id": "d1", "filename": "main.rs", "hunks": [{"header": "@@ -1,3 +1,4 @@", "lines": [{"kind": "context", "content": "use std;"}, {"kind": "add", "content": "use serde;"}]}]}
  ],
  "media": [
    {"id": "img1", "media_type": "image", "data": "base64...", "mime": "image/png"}
  ],
  "actions": [
    {"id": "act1", "label": "Approve", "style": "primary", "payload": {"type": "send-message", "text": "approved"}}
  ]
}
</a2ui>

Components render natively on each transport:
- Desktop: React components (cards, tables, syntax-highlighted code)
- CLI: ANSI-formatted tables, colored alerts, ASCII progress bars
- Discord: Embeds with colors and fields
- Slack: Block Kit with headers, sections, buttons
- Telegram: HTML formatting with inline keyboards
- Plain: Tab-separated fallback

Mix plain text with <a2ui> blocks freely. Use components when structure aids comprehension.
</a2ui_protocol>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_table_render() {
        let resp = A2uiResponse {
            text: String::new(),
            components: vec![Component::Table {
                id: "t1".into(),
                headers: vec!["Name".into(), "Score".into()],
                rows: vec![
                    vec!["Alice".into(), "95".into()],
                    vec!["Bob".into(), "87".into()],
                ],
                caption: Some("Results".into()),
            }],
            media: vec![],
            actions: vec![],
        };
        let out = render(&resp, Transport::Cli);
        assert!(out.contains("Results"));
        assert!(out.contains("Alice"));
        assert!(out.contains("│"));
    }

    #[test]
    fn test_cli_alert_colors() {
        let resp = A2uiResponse {
            text: String::new(),
            components: vec![Component::Alert {
                id: "a1".into(),
                level: AlertLevel::Error,
                title: "Build failed".into(),
                body: Some("Exit code 1".into()),
            }],
            media: vec![],
            actions: vec![],
        };
        let out = render(&resp, Transport::Cli);
        assert!(out.contains("\x1b[31m")); // Red
        assert!(out.contains("Build failed"));
    }

    #[test]
    fn test_discord_embeds() {
        let resp = A2uiResponse {
            text: "Summary".into(),
            components: vec![Component::Card {
                id: "c1".into(),
                title: "Status".into(),
                subtitle: None,
                fields: vec![Field { label: "CPU".into(), value: "42%".into(), inline: true }],
                image_url: None,
                color: Some("#2ecc71".into()),
            }],
            media: vec![],
            actions: vec![],
        };
        let out = render(&resp, Transport::Discord);
        assert!(out.contains("embeds"));
        assert!(out.contains("Status"));
    }

    #[test]
    fn test_slack_blocks() {
        let resp = A2uiResponse {
            text: "Hello".into(),
            components: vec![],
            media: vec![],
            actions: vec![Action {
                id: "a1".into(),
                label: "Click me".into(),
                style: ActionStyle::Primary,
                payload: ActionPayload::SendMessage { text: "clicked".into() },
            }],
        };
        let out = render(&resp, Transport::Slack);
        assert!(out.contains("blocks"));
        assert!(out.contains("Click me"));
    }

    #[test]
    fn test_extract_from_text() {
        let text = r#"Here's the status:

<a2ui>
{"text":"ok","components":[{"type":"metric","id":"m1","label":"Uptime","value":"99.9%","trend":"up"}]}
</a2ui>

Done."#;
        let blocks = extract_a2ui_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].components.len(), 1);
    }

    #[test]
    fn test_progress_bar_cli() {
        let resp = A2uiResponse {
            text: String::new(),
            components: vec![Component::Progress {
                id: "p1".into(), label: "Build".into(), value: 7.0, max: 10.0, status: None,
            }],
            media: vec![], actions: vec![],
        };
        let out = render(&resp, Transport::Cli);
        assert!(out.contains("█"));
        assert!(out.contains("70%"));
    }
}
