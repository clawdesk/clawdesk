//! Enhanced snapshots — ARIA-tree, AI-optimized, and ref-based element targeting.
//!
//! Three snapshot modes:
//! - **DOM** (default): Element-indexed via `data-ci` attributes (existing `dom_intel`)
//! - **ARIA**: Full accessibility tree via CDP `Accessibility.getFullAXTree`
//! - **AI**: Compact LLM-optimized snapshot with ref-based element targeting
//!
//! ## Ref-based targeting
//! Each interactive element gets a stable `[ref=eN]` identifier injected as
//! `data-ref` attribute, enabling click/type by reference even after DOM changes.

use crate::cdp::CdpSession;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Snapshot mode for page analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotMode {
    /// DOM Intelligence: data-ci indexed elements + page text (default).
    Dom,
    /// Accessibility tree: full ARIA tree from CDP.
    Aria,
    /// AI-optimized: compact snapshot with ref-based targeting.
    Ai,
}

impl Default for SnapshotMode {
    fn default() -> Self {
        Self::Dom
    }
}

impl std::str::FromStr for SnapshotMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "dom" => Ok(Self::Dom),
            "aria" | "accessibility" => Ok(Self::Aria),
            "ai" | "compact" => Ok(Self::Ai),
            _ => Err(format!("unknown snapshot mode: '{}' (use dom/aria/ai)", s)),
        }
    }
}

/// Configuration for snapshot extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotConfig {
    /// Maximum characters in the output (truncation point).
    pub max_chars: usize,
    /// Whether to include non-interactive elements.
    pub include_non_interactive: bool,
    /// Maximum tree depth for ARIA mode.
    pub max_depth: usize,
    /// Whether to include text content snippets.
    pub include_text: bool,
    /// Whether to include page metadata (title, URL, etc.).
    pub include_metadata: bool,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            max_chars: 50_000,
            include_non_interactive: false,
            max_depth: 30,
            include_text: true,
            include_metadata: true,
        }
    }
}

/// An enhanced page snapshot with multiple output formats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedSnapshot {
    /// Page URL.
    pub url: String,
    /// Page title.
    pub title: String,
    /// Snapshot mode used.
    pub mode: SnapshotMode,
    /// Formatted snapshot text (for LLM consumption).
    pub content: String,
    /// Number of interactive elements found.
    pub interactive_count: usize,
    /// Total elements in the tree.
    pub total_elements: usize,
    /// Whether the output was truncated.
    pub truncated: bool,
}

// ── ARIA Snapshot ──────────────────────────────────────────

/// Extract an ARIA accessibility tree snapshot.
///
/// Uses CDP `Accessibility.getFullAXTree` for a complete accessibility view.
/// Better for screen-reader-like understanding of the page.
pub async fn aria_snapshot(
    cdp: &CdpSession,
    config: &SnapshotConfig,
) -> Result<EnhancedSnapshot, String> {
    // Get page metadata
    let url = eval_string(cdp, "window.location.href").await?;
    let title = eval_string(cdp, "document.title").await?;

    // Fetch the full accessibility tree
    let cmd = cdp.build_command(
        "Accessibility.getFullAXTree",
        serde_json::json!({ "depth": config.max_depth }),
    );
    let resp = cdp.send(cmd).await?;

    let nodes = resp
        .result
        .as_ref()
        .and_then(|r| r.get("nodes"))
        .and_then(|n| n.as_array())
        .ok_or("no accessibility nodes in response")?;

    let total_elements = nodes.len();
    let mut interactive_count = 0;
    let mut output = String::new();

    if config.include_metadata {
        output.push_str(&format!("Page: {} | {}\n", title, url));
        output.push_str(&format!("Accessibility Tree ({} nodes):\n\n", total_elements));
    }

    for node in nodes {
        let role = node
            .get("role")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // Skip ignored nodes
        if node.get("ignored").and_then(|v| v.as_bool()).unwrap_or(false) {
            continue;
        }

        let name = node
            .get("name")
            .and_then(|n| n.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let is_interactive = is_interactive_role(role);
        if is_interactive {
            interactive_count += 1;
        }

        if !config.include_non_interactive && !is_interactive && name.is_empty() {
            continue;
        }

        // Compute depth from parent chain (approximate via nodeId)
        let depth = node
            .get("depth")
            .and_then(|d| d.as_u64())
            .unwrap_or(0) as usize;
        let indent = "  ".repeat(depth.min(config.max_depth));

        let mut line = format!("{}{}", indent, role);
        if !name.is_empty() {
            line.push_str(&format!(" \"{}\"", truncate_str(name, 100)));
        }

        // Add properties
        if let Some(props) = node.get("properties").and_then(|p| p.as_array()) {
            let interesting: Vec<String> = props
                .iter()
                .filter_map(|p| {
                    let n = p.get("name")?.as_str()?;
                    let v = p.get("value")?.get("value")?;
                    match n {
                        "disabled" | "checked" | "selected" | "expanded" | "required"
                        | "invalid" | "focused" => {
                            if v.as_bool().unwrap_or(false) {
                                Some(n.to_string())
                            } else {
                                None
                            }
                        }
                        "value" => Some(format!("value={}", truncate_str(&v.to_string(), 50))),
                        _ => None,
                    }
                })
                .collect();
            if !interesting.is_empty() {
                line.push_str(&format!(" [{}]", interesting.join(", ")));
            }
        }

        output.push_str(&line);
        output.push('\n');

        // Truncation check
        if output.len() > config.max_chars {
            output.truncate(config.max_chars);
            output.push_str("\n...[truncated]");
            return Ok(EnhancedSnapshot {
                url,
                title,
                mode: SnapshotMode::Aria,
                content: output,
                interactive_count,
                total_elements,
                truncated: true,
            });
        }
    }

    debug!(
        nodes = total_elements,
        interactive = interactive_count,
        "ARIA snapshot extracted"
    );

    Ok(EnhancedSnapshot {
        url,
        title,
        mode: SnapshotMode::Aria,
        content: output,
        interactive_count,
        total_elements,
        truncated: false,
    })
}

// ── AI Snapshot (ref-based) ────────────────────────────────

/// JavaScript to inject ref-based element markers and extract a compact snapshot.
const AI_SNAPSHOT_JS: &str = r#"(() => {
    // Assign data-ref="eN" to interactive elements
    const interactive = document.querySelectorAll(
        'a[href], button, input, textarea, select, [role="button"], [role="link"], ' +
        '[role="checkbox"], [role="radio"], [role="tab"], [role="menuitem"], ' +
        '[role="switch"], [role="slider"], [role="combobox"], [role="searchbox"], ' +
        '[contenteditable="true"], details > summary'
    );

    let refIdx = 0;
    const elements = [];

    for (const el of interactive) {
        // Skip hidden elements
        if (el.offsetParent === null && el.tagName !== 'BODY' && !el.closest('details')) continue;
        if (getComputedStyle(el).visibility === 'hidden') continue;

        const ref = 'e' + refIdx;
        el.setAttribute('data-ref', ref);
        refIdx++;

        // Build accessible label
        const ariaLabel = el.getAttribute('aria-label') || '';
        const title = el.getAttribute('title') || '';
        const placeholder = el.getAttribute('placeholder') || '';
        const text = (el.textContent || '').trim().slice(0, 80);
        const label = ariaLabel || title || placeholder || text || el.tagName.toLowerCase();

        const info = {
            ref: ref,
            tag: el.tagName.toLowerCase(),
            type: el.getAttribute('type') || '',
            role: el.getAttribute('role') || '',
            label: label,
            href: el.getAttribute('href') || undefined,
            value: (el.value || '').slice(0, 50) || undefined,
            disabled: el.disabled || false,
            checked: el.checked || undefined,
        };

        // Clean undefined values
        Object.keys(info).forEach(k => info[k] === undefined && delete info[k]);
        elements.push(info);
    }

    // Extract visible text content (compact)
    const headings = Array.from(document.querySelectorAll('h1,h2,h3,h4,h5,h6')).slice(0, 20).map(h => ({
        level: parseInt(h.tagName[1]),
        text: h.textContent.trim().slice(0, 120)
    }));

    const bodyText = (document.body?.innerText || '').slice(0, __MAX_TEXT__);

    return {
        url: window.location.href,
        title: document.title,
        elements: elements,
        headings: headings,
        bodyText: bodyText,
        scrollY: Math.round(scrollY),
        scrollHeight: Math.round(document.documentElement.scrollHeight),
        viewportHeight: window.innerHeight
    };
})()"#;

/// Extract an AI-optimized snapshot with ref-based element targeting.
///
/// Injects `data-ref="eN"` attributes on interactive elements and returns
/// a compact representation optimized for LLM token efficiency.
pub async fn ai_snapshot(
    cdp: &CdpSession,
    config: &SnapshotConfig,
) -> Result<EnhancedSnapshot, String> {
    // Configure max text based on config
    let text_budget = config.max_chars / 2; // Half budget for body text
    let js = AI_SNAPSHOT_JS.replace("__MAX_TEXT__", &text_budget.to_string());

    let result = cdp.eval(&js).await?;

    // Parse the result
    let data = result
        .get("result")
        .and_then(|r| r.get("value"))
        .unwrap_or(&result);

    let url = data
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let title = data
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let elements = data
        .get("elements")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let headings = data
        .get("headings")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let body_text = data
        .get("bodyText")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let scroll_y = data.get("scrollY").and_then(|v| v.as_i64()).unwrap_or(0);
    let scroll_height = data
        .get("scrollHeight")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let viewport_height = data
        .get("viewportHeight")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let interactive_count = elements.len();
    let total_elements = interactive_count;

    // Format for LLM
    let mut output = String::new();

    if config.include_metadata {
        output.push_str(&format!("Page: {} | {}\n", title, url));
        output.push_str(&format!(
            "Scroll: {}px / {}px (viewport: {}px)\n\n",
            scroll_y, scroll_height, viewport_height
        ));
    }

    // Headings (structure)
    if !headings.is_empty() {
        output.push_str("Structure:\n");
        for h in &headings {
            let level = h.get("level").and_then(|v| v.as_u64()).unwrap_or(0);
            let text = h.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let indent = "  ".repeat(level as usize - 1);
            output.push_str(&format!("{}h{}: {}\n", indent, level, text));
        }
        output.push('\n');
    }

    // Interactive elements
    if !elements.is_empty() {
        output.push_str(&format!("Interactive elements ({}):\n", elements.len()));
        for el in &elements {
            let ref_id = el.get("ref").and_then(|v| v.as_str()).unwrap_or("?");
            let tag = el.get("tag").and_then(|v| v.as_str()).unwrap_or("?");
            let label = el.get("label").and_then(|v| v.as_str()).unwrap_or("");
            let role = el.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let input_type = el.get("type").and_then(|v| v.as_str()).unwrap_or("");

            let mut desc = format!("  [{}] {}", ref_id, tag);
            if !role.is_empty() {
                desc.push_str(&format!("[role={}]", role));
            }
            if !input_type.is_empty() {
                desc.push_str(&format!("[type={}]", input_type));
            }
            desc.push_str(&format!(" \"{}\"", truncate_str(label, 60)));

            // Additional attributes
            if let Some(href) = el.get("href").and_then(|v| v.as_str()) {
                desc.push_str(&format!(" → {}", truncate_str(href, 60)));
            }
            if let Some(value) = el.get("value").and_then(|v| v.as_str()) {
                if !value.is_empty() {
                    desc.push_str(&format!(" (value: {})", truncate_str(value, 30)));
                }
            }
            if el.get("disabled").and_then(|v| v.as_bool()).unwrap_or(false) {
                desc.push_str(" [disabled]");
            }

            output.push_str(&desc);
            output.push('\n');
        }
        output.push('\n');
    }

    // Body text (truncated)
    if config.include_text && !body_text.is_empty() {
        output.push_str("Page text:\n");
        let remaining = config.max_chars.saturating_sub(output.len());
        if body_text.len() > remaining {
            output.push_str(&body_text[..remaining]);
            output.push_str("\n...[truncated]");
        } else {
            output.push_str(body_text);
        }
    }

    let truncated = output.len() >= config.max_chars;

    debug!(
        elements = interactive_count,
        mode = "ai",
        "AI snapshot extracted"
    );

    Ok(EnhancedSnapshot {
        url,
        title,
        mode: SnapshotMode::Ai,
        content: output,
        interactive_count,
        total_elements,
        truncated,
    })
}

/// Perform a click-by-ref action using `data-ref` attribute.
///
/// This is the counterpart to the AI snapshot — elements are targeted by
/// their `[ref=eN]` identifier instead of index or CSS selector.
pub async fn click_by_ref(cdp: &CdpSession, ref_id: &str) -> Result<serde_json::Value, String> {
    let js = format!(
        r#"(() => {{
            const el = document.querySelector('[data-ref="{}"]');
            if (!el) return {{ success: false, error: 'ref [{}] not found — page may have changed' }};
            el.scrollIntoView({{ block: 'center' }});
            el.click();
            return {{ success: true, tag: el.tagName, text: (el.textContent||'').trim().slice(0,60) }};
        }})()"#,
        escape_ref(ref_id),
        escape_ref(ref_id),
    );
    cdp.eval(&js).await
}

/// Type into an element by its ref ID.
pub async fn type_by_ref(
    cdp: &CdpSession,
    ref_id: &str,
    text: &str,
    clear: bool,
) -> Result<serde_json::Value, String> {
    let escaped_text = escape_js(text);
    let clear_js = if clear { "el.value = '';" } else { "" };
    let assign = if clear { "=" } else { "+=" };

    let js = format!(
        r#"(() => {{
            const el = document.querySelector('[data-ref="{}"]');
            if (!el) return {{ success: false, error: 'ref [{}] not found' }};
            el.focus();
            {clear_js}
            el.value {assign} '{escaped_text}';
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return {{ success: true, value: el.value.slice(0, 60) }};
        }})()"#,
        escape_ref(ref_id),
        escape_ref(ref_id),
        clear_js = clear_js,
        assign = assign,
        escaped_text = escaped_text,
    );
    cdp.eval(&js).await
}

// ── Helpers ──────────────────────────────────────────────────

/// Check if a role is interactive (clickable/typeable/selectable).
fn is_interactive_role(role: &str) -> bool {
    matches!(
        role,
        "button"
            | "link"
            | "textbox"
            | "searchbox"
            | "combobox"
            | "checkbox"
            | "radio"
            | "tab"
            | "menuitem"
            | "switch"
            | "slider"
            | "spinbutton"
            | "option"
            | "treeitem"
            | "gridcell"
            | "menuitemcheckbox"
            | "menuitemradio"
    )
}

/// Truncate a string to a maxlen, adding "…" if truncated.
fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        // Find a char boundary
        let mut end = max;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        &s[..end]
    }
}

/// Evaluate a JS expression and return the string value.
async fn eval_string(cdp: &CdpSession, expr: &str) -> Result<String, String> {
    let val = cdp.eval(expr).await?;
    Ok(val
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string())
}

/// Escape a ref ID for safe embedding in a JS attribute selector.
fn escape_ref(ref_id: &str) -> String {
    ref_id
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect()
}

/// Escape a string for JS single-quoted string literal.
fn escape_js(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            c if c.is_control() => {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_mode_parse() {
        assert_eq!("dom".parse::<SnapshotMode>().unwrap(), SnapshotMode::Dom);
        assert_eq!("aria".parse::<SnapshotMode>().unwrap(), SnapshotMode::Aria);
        assert_eq!("ai".parse::<SnapshotMode>().unwrap(), SnapshotMode::Ai);
        assert_eq!(
            "accessibility".parse::<SnapshotMode>().unwrap(),
            SnapshotMode::Aria
        );
        assert!("invalid".parse::<SnapshotMode>().is_err());
    }

    #[test]
    fn default_config() {
        let c = SnapshotConfig::default();
        assert_eq!(c.max_chars, 50_000);
        assert_eq!(c.max_depth, 30);
        assert!(!c.include_non_interactive);
        assert!(c.include_text);
        assert!(c.include_metadata);
    }

    #[test]
    fn truncate_str_short() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_long() {
        assert_eq!(truncate_str("hello world", 5), "hello");
    }

    #[test]
    fn escape_ref_safe() {
        assert_eq!(escape_ref("e42"), "e42");
        assert_eq!(escape_ref("e1-test_2"), "e1-test_2");
        // Malicious input stripped
        assert_eq!(escape_ref(r#"e1"]); alert(1); //"#), "e1alert1");
    }

    #[test]
    fn interactive_roles() {
        assert!(is_interactive_role("button"));
        assert!(is_interactive_role("textbox"));
        assert!(is_interactive_role("link"));
        assert!(!is_interactive_role("generic"));
        assert!(!is_interactive_role("paragraph"));
        assert!(!is_interactive_role("heading"));
    }
}
