//! Browser tool implementations — 7 tools for LLM-driven browser automation.
//!
//! Each tool implements ClawDesk's `Tool` trait with `Arc<BrowserManager>`
//! dependency injection. Tools use index-based element targeting via DOM
//! intelligence `data-ci` attributes.
//!
//! ## Tools
//! 1. `browser_observe` — DOM intelligence extraction
//! 2. `browser_navigate` — Navigate to URL with SSRF protection
//! 3. `browser_click` — Index-based element clicking
//! 4. `browser_type` — Index-based text input
//! 5. `browser_screenshot` — Page screenshot
//! 6. `browser_scroll` — Scroll up/down
//! 7. `browser_close` — Close session

use crate::tools::{Tool, ToolCapability, ToolSchema};
use async_trait::async_trait;
use clawdesk_browser::manager::BrowserManager;
use clawdesk_browser::{dom_intel, file_ops, safety, snapshot, ssrf, tabs};
use serde_json::json;
use std::sync::Arc;
use tracing::warn;

/// Escape a string for safe interpolation into JavaScript string literals.
fn escape_js_string(s: &str) -> String {
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

// ═══════════════════════════════════════════════════════════════
// Tool 1: browser_observe — DOM Intelligence extraction
// ═══════════════════════════════════════════════════════════════

/// Analyze the current page and return numbered interactive elements.
///
/// This is THE primary browser perception tool. The LLM should call this
/// after every navigation or page state change to get an indexed list
/// of clickable/typeable elements.
pub struct BrowserObserveTool {
    manager: Arc<BrowserManager>,
    agent_id: String,
}

impl BrowserObserveTool {
    pub fn new(manager: Arc<BrowserManager>, agent_id: String) -> Self {
        Self { manager, agent_id }
    }
}

#[async_trait]
impl Tool for BrowserObserveTool {
    fn name(&self) -> &str {
        "browser_observe"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_observe".into(),
            description: "Analyze the current page and return a numbered list of all \
                interactive elements (buttons, links, inputs, etc.) plus page content. \
                ALWAYS call this after navigation or when the page changes. Use the \
                element [index] numbers in subsequent click/type actions."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        let session = self.manager.get_or_create(&self.agent_id).await?;
        let mut s = session.lock().await;
        s.last_active = std::time::Instant::now();

        let snapshot = dom_intel::extract_dom_intelligence(&s.cdp).await?;
        let formatted = snapshot.format_for_llm();

        if self.manager.config.wrap_external_content {
            Ok(safety::wrap_browser_content(
                &snapshot.url,
                &snapshot.title,
                &formatted,
            ))
        } else {
            Ok(formatted)
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 2: browser_navigate — Navigate to URL with SSRF protection
// ═══════════════════════════════════════════════════════════════

pub struct BrowserNavigateTool {
    manager: Arc<BrowserManager>,
    agent_id: String,
}

impl BrowserNavigateTool {
    pub fn new(manager: Arc<BrowserManager>, agent_id: String) -> Self {
        Self { manager, agent_id }
    }
}

#[async_trait]
impl Tool for BrowserNavigateTool {
    fn name(&self) -> &str {
        "browser_navigate"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_navigate".into(),
            description: "Navigate to a URL. After navigation completes, automatically \
                returns the page observation (same as browser_observe). No need to call \
                browser_observe separately after navigating."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to navigate to (http/https only)"
                    }
                },
                "required": ["url"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser, ToolCapability::Network]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or("missing 'url' parameter")?;

        // SSRF gate — validate before any network activity
        ssrf::check_ssrf(url, &self.manager.config)?;

        let session = self.manager.get_or_create(&self.agent_id).await?;
        let mut s = session.lock().await;
        s.last_active = std::time::Instant::now();

        // Check page limit
        s.pages_visited += 1;
        if s.pages_visited > self.manager.config.max_pages_per_task {
            return Err(format!(
                "page limit reached ({}/{})",
                s.pages_visited, self.manager.config.max_pages_per_task
            ));
        }

        // Navigate
        s.cdp.navigate_and_wait(url).await?;

        // Wait for dynamic content (brief delay for JS rendering)
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Auto-observe after navigation
        let snapshot = dom_intel::extract_dom_intelligence(&s.cdp).await?;
        let formatted = snapshot.format_for_llm();

        if self.manager.config.wrap_external_content {
            Ok(safety::wrap_browser_content(
                &snapshot.url,
                &snapshot.title,
                &formatted,
            ))
        } else {
            Ok(formatted)
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 3: browser_click — Index-based element clicking
// ═══════════════════════════════════════════════════════════════

pub struct BrowserClickTool {
    manager: Arc<BrowserManager>,
    agent_id: String,
}

impl BrowserClickTool {
    pub fn new(manager: Arc<BrowserManager>, agent_id: String) -> Self {
        Self { manager, agent_id }
    }
}

#[async_trait]
impl Tool for BrowserClickTool {
    fn name(&self) -> &str {
        "browser_click"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_click".into(),
            description: "Click an interactive element by its [index] number from \
                the most recent browser_observe or browser_navigate result. \
                Prefer using 'index' over 'selector'."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "index": {
                        "type": "integer",
                        "description": "Element index from the observation list (preferred)"
                    },
                    "selector": {
                        "type": "string",
                        "description": "CSS selector (fallback only — use index when available)"
                    }
                }
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let session = self.manager.get_or_create(&self.agent_id).await?;
        let mut s = session.lock().await;
        s.last_active = std::time::Instant::now();

        // Determine click target
        let js = if let Some(index) = args.get("index").and_then(|v| v.as_u64()) {
            // Index-based: O(1) attribute lookup
            format!(
                r#"(() => {{
                    const el = document.querySelector('[data-ci="{}"]');
                    if (!el) return {{ success: false, error: 'element [{}] not found — page may have changed, call browser_observe again' }};
                    el.scrollIntoView({{ block: 'center' }});
                    el.click();
                    return {{ success: true, tag: el.tagName, text: (el.textContent||'').trim().slice(0,60) }};
                }})()"#,
                index, index
            )
        } else if let Some(selector) = args.get("selector").and_then(|v| v.as_str()) {
            // CSS selector fallback
            let escaped = escape_js_string(selector);
            format!(
                r#"(() => {{
                    const el = document.querySelector('{}');
                    if (!el) return {{ success: false, error: 'selector not found: {}' }};
                    el.scrollIntoView({{ block: 'center' }});
                    el.click();
                    return {{ success: true, tag: el.tagName, text: (el.textContent||'').trim().slice(0,60) }};
                }})()"#,
                escaped, escaped
            )
        } else {
            return Err("provide either 'index' (preferred) or 'selector'".to_string());
        };

        let result = s.cdp.eval(&js).await?;

        // Check if this was a purchase action
        let clicked_text = result
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        let page_title = s
            .cdp
            .eval("document.title")
            .await
            .ok()
            .and_then(|v| {
                v.get("result")
                    .and_then(|r| r.get("value"))
                    .and_then(|v| v.as_str().map(String::from))
            })
            .unwrap_or_default();

        if safety::is_purchase_action(clicked_text, &page_title) {
            warn!(
                agent_id = self.agent_id,
                element = clicked_text,
                "purchase action detected on click"
            );
        }

        // Brief wait for page reaction
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let success = result
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if success {
            Ok(format!(
                "Clicked {} '{}'. Call browser_observe to see the updated page.",
                result
                    .get("tag")
                    .and_then(|t| t.as_str())
                    .unwrap_or("element"),
                clicked_text
            ))
        } else {
            let error = result
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown error");
            Err(error.to_string())
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 4: browser_type — Index-based text input
// ═══════════════════════════════════════════════════════════════

pub struct BrowserTypeTool {
    manager: Arc<BrowserManager>,
    agent_id: String,
}

impl BrowserTypeTool {
    pub fn new(manager: Arc<BrowserManager>, agent_id: String) -> Self {
        Self { manager, agent_id }
    }
}

#[async_trait]
impl Tool for BrowserTypeTool {
    fn name(&self) -> &str {
        "browser_type"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_type".into(),
            description: "Type text into an input element by its [index] number. \
                Clears existing value before typing. Use 'append: true' to add to existing text."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "index": {
                        "type": "integer",
                        "description": "Element index from the observation list (preferred)"
                    },
                    "selector": {
                        "type": "string",
                        "description": "CSS selector (fallback)"
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to type into the element"
                    },
                    "append": {
                        "type": "boolean",
                        "description": "If true, append to existing value instead of replacing"
                    },
                    "press_enter": {
                        "type": "boolean",
                        "description": "If true, press Enter after typing (for search forms)"
                    }
                },
                "required": ["text"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or("missing 'text' parameter")?;
        let append = args
            .get("append")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let press_enter = args
            .get("press_enter")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let escaped_text = escape_js_string(text);

        let session = self.manager.get_or_create(&self.agent_id).await?;
        let mut s = session.lock().await;
        s.last_active = std::time::Instant::now();

        // Build querySelector expression based on index or selector
        let finder = if let Some(index) = args.get("index").and_then(|v| v.as_u64()) {
            format!(r#"document.querySelector('[data-ci="{}"]')"#, index)
        } else if let Some(selector) = args.get("selector").and_then(|v| v.as_str()) {
            let escaped = escape_js_string(selector);
            format!(r#"document.querySelector('{}')"#, escaped)
        } else {
            return Err("provide either 'index' or 'selector'".to_string());
        };

        let clear = if append { "" } else { "el.value = '';" };
        let assign = if append { "+=" } else { "=" };
        let enter = if press_enter {
            "el.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })); \
             if (el.form) el.form.submit();"
        } else {
            ""
        };

        let js = format!(
            r#"(() => {{
                const el = {finder};
                if (!el) return {{ success: false, error: 'element not found' }};
                el.focus();
                {clear}
                el.value {assign} '{escaped_text}';
                el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                {enter}
                return {{ success: true, value: el.value.slice(0, 60) }};
            }})()"#,
            finder = finder,
            clear = clear,
            assign = assign,
            escaped_text = escaped_text,
            enter = enter,
        );

        let result = s.cdp.eval(&js).await?;
        let success = result
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if success {
            Ok(format!(
                "Typed '{}' into element. Current value: '{}'",
                text,
                result.get("value").and_then(|v| v.as_str()).unwrap_or("")
            ))
        } else {
            let error = result
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown");
            Err(error.to_string())
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 5: browser_screenshot
// ═══════════════════════════════════════════════════════════════

pub struct BrowserScreenshotTool {
    manager: Arc<BrowserManager>,
    agent_id: String,
}

impl BrowserScreenshotTool {
    pub fn new(manager: Arc<BrowserManager>, agent_id: String) -> Self {
        Self { manager, agent_id }
    }
}

#[async_trait]
impl Tool for BrowserScreenshotTool {
    fn name(&self) -> &str {
        "browser_screenshot"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_screenshot".into(),
            description: "Take a screenshot of the current page. Returns base64-encoded PNG. \
                Use for visual verification when the page observation doesn't provide enough context."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "full_page": {
                        "type": "boolean",
                        "description": "Capture full page (true) or just viewport (false, default)"
                    }
                }
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        let session = self.manager.get_or_create(&self.agent_id).await?;
        let mut s = session.lock().await;
        s.last_active = std::time::Instant::now();

        let b64 = s.cdp.take_screenshot().await?;
        Ok(format!("data:image/png;base64,{}", b64))
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 6: browser_scroll
// ═══════════════════════════════════════════════════════════════

pub struct BrowserScrollTool {
    manager: Arc<BrowserManager>,
    agent_id: String,
}

impl BrowserScrollTool {
    pub fn new(manager: Arc<BrowserManager>, agent_id: String) -> Self {
        Self { manager, agent_id }
    }
}

#[async_trait]
impl Tool for BrowserScrollTool {
    fn name(&self) -> &str {
        "browser_scroll"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_scroll".into(),
            description: "Scroll the page up or down. After scrolling, call browser_observe \
                to see the newly visible elements."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "direction": {
                        "type": "string",
                        "enum": ["up", "down"],
                        "description": "Scroll direction"
                    },
                    "amount": {
                        "type": "integer",
                        "description": "Pixels to scroll (default: one viewport height)"
                    }
                },
                "required": ["direction"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let direction = args
            .get("direction")
            .and_then(|v| v.as_str())
            .ok_or("missing 'direction'")?;
        let amount = args
            .get("amount")
            .and_then(|v| v.as_i64())
            .unwrap_or(0); // 0 = use viewport height

        let session = self.manager.get_or_create(&self.agent_id).await?;
        let mut s = session.lock().await;
        s.last_active = std::time::Instant::now();

        let js = format!(
            r#"(() => {{
                const amt = {} || window.innerHeight;
                const dir = '{}' === 'up' ? -amt : amt;
                window.scrollBy(0, dir);
                return {{ scrollY: Math.round(scrollY), maxY: Math.round(document.documentElement.scrollHeight) }};
            }})()"#,
            amount, direction
        );

        let result = s.cdp.eval(&js).await?;
        let scroll_y = result
            .get("scrollY")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let max_y = result.get("maxY").and_then(|v| v.as_i64()).unwrap_or(0);

        Ok(format!(
            "Scrolled {}. Position: {}/{} px. Call browser_observe to see updated elements.",
            direction, scroll_y, max_y
        ))
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 7: browser_close
// ═══════════════════════════════════════════════════════════════

pub struct BrowserCloseTool {
    manager: Arc<BrowserManager>,
    agent_id: String,
}

impl BrowserCloseTool {
    pub fn new(manager: Arc<BrowserManager>, agent_id: String) -> Self {
        Self { manager, agent_id }
    }
}

#[async_trait]
impl Tool for BrowserCloseTool {
    fn name(&self) -> &str {
        "browser_close"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_close".into(),
            description: "Close the browser session and free resources. \
                Call when the browsing task is complete."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        self.manager.close_session(&self.agent_id).await;
        Ok("Browser session closed.".to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 8: browser_tab_list — List open tabs
// ═══════════════════════════════════════════════════════════════

pub struct BrowserTabListTool {
    manager: Arc<BrowserManager>,
    agent_id: String,
}

impl BrowserTabListTool {
    pub fn new(manager: Arc<BrowserManager>, agent_id: String) -> Self {
        Self { manager, agent_id }
    }
}

#[async_trait]
impl Tool for BrowserTabListTool {
    fn name(&self) -> &str {
        "browser_tab_list"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_tab_list".into(),
            description: "List all open browser tabs with their titles and URLs.".into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        let session = self.manager.get_or_create(&self.agent_id).await?;
        let s = session.lock().await;
        let tab_list = tabs::list_tabs(&s.cdp, false).await?;
        Ok(tabs::format_tabs_for_llm(&tab_list))
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 9: browser_tab_open — Open a new tab
// ═══════════════════════════════════════════════════════════════

pub struct BrowserTabOpenTool {
    manager: Arc<BrowserManager>,
    agent_id: String,
}

impl BrowserTabOpenTool {
    pub fn new(manager: Arc<BrowserManager>, agent_id: String) -> Self {
        Self { manager, agent_id }
    }
}

#[async_trait]
impl Tool for BrowserTabOpenTool {
    fn name(&self) -> &str {
        "browser_tab_open"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_tab_open".into(),
            description: "Open a new browser tab. Optionally navigate to a URL.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL to open (defaults to about:blank)"
                    }
                }
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let url = args.get("url").and_then(|v| v.as_str());

        // SSRF check if URL provided
        if let Some(u) = url {
            ssrf::check_ssrf(u, &self.manager.config)?;
        }

        let session = self.manager.get_or_create(&self.agent_id).await?;
        let s = session.lock().await;
        let result = tabs::open_tab(&s.cdp, url).await?;
        Ok(format!(
            "Opened new tab (id: {}) at {}",
            result.target_id, result.url
        ))
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 10: browser_tab_close — Close a tab
// ═══════════════════════════════════════════════════════════════

pub struct BrowserTabCloseTool {
    manager: Arc<BrowserManager>,
    agent_id: String,
}

impl BrowserTabCloseTool {
    pub fn new(manager: Arc<BrowserManager>, agent_id: String) -> Self {
        Self { manager, agent_id }
    }
}

#[async_trait]
impl Tool for BrowserTabCloseTool {
    fn name(&self) -> &str {
        "browser_tab_close"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_tab_close".into(),
            description: "Close a browser tab by its index from browser_tab_list.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "tab_index": {
                        "type": "integer",
                        "description": "Tab index from browser_tab_list"
                    }
                },
                "required": ["tab_index"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let idx = args
            .get("tab_index")
            .and_then(|v| v.as_u64())
            .ok_or("missing 'tab_index'")?
            as usize;

        let session = self.manager.get_or_create(&self.agent_id).await?;
        let s = session.lock().await;
        let tab_list = tabs::list_tabs(&s.cdp, false).await?;
        let tab = tab_list
            .get(idx)
            .ok_or_else(|| format!("tab index {} out of range (0-{})", idx, tab_list.len().saturating_sub(1)))?;

        let success = tabs::close_tab(&s.cdp, &tab.target_id).await?;
        if success {
            Ok(format!("Closed tab [{}] '{}'", idx, tab.title))
        } else {
            Err(format!("Failed to close tab [{}]", idx))
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 11: browser_snapshot — Enhanced AI/ARIA snapshot
// ═══════════════════════════════════════════════════════════════

pub struct BrowserSnapshotTool {
    manager: Arc<BrowserManager>,
    agent_id: String,
}

impl BrowserSnapshotTool {
    pub fn new(manager: Arc<BrowserManager>, agent_id: String) -> Self {
        Self { manager, agent_id }
    }
}

#[async_trait]
impl Tool for BrowserSnapshotTool {
    fn name(&self) -> &str {
        "browser_snapshot"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_snapshot".into(),
            description: "Take an enhanced snapshot of the page. Modes: 'ai' (compact, \
                ref-based elements), 'aria' (accessibility tree), 'dom' (default indexed). \
                AI mode assigns [ref=eN] identifiers for precise element targeting."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "enum": ["ai", "aria", "dom"],
                        "description": "Snapshot mode (default: 'ai')"
                    },
                    "max_chars": {
                        "type": "integer",
                        "description": "Maximum output characters (default: 50000)"
                    }
                }
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let mode_str = args
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("ai");
        let mode: snapshot::SnapshotMode = mode_str
            .parse()
            .map_err(|e: String| e)?;
        let max_chars = args
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(50_000) as usize;

        let session = self.manager.get_or_create(&self.agent_id).await?;
        let mut s = session.lock().await;
        s.last_active = std::time::Instant::now();

        let config = snapshot::SnapshotConfig {
            max_chars,
            ..Default::default()
        };

        match mode {
            snapshot::SnapshotMode::Ai => {
                let snap = snapshot::ai_snapshot(&s.cdp, &config).await?;
                if self.manager.config.wrap_external_content {
                    Ok(safety::wrap_browser_content(&snap.url, &snap.title, &snap.content))
                } else {
                    Ok(snap.content)
                }
            }
            snapshot::SnapshotMode::Aria => {
                let snap = snapshot::aria_snapshot(&s.cdp, &config).await?;
                if self.manager.config.wrap_external_content {
                    Ok(safety::wrap_browser_content(&snap.url, &snap.title, &snap.content))
                } else {
                    Ok(snap.content)
                }
            }
            snapshot::SnapshotMode::Dom => {
                // Fall back to existing DOM intelligence
                let dom_snap = dom_intel::extract_dom_intelligence(&s.cdp).await?;
                let formatted = dom_snap.format_for_llm();
                if self.manager.config.wrap_external_content {
                    Ok(safety::wrap_browser_content(&dom_snap.url, &dom_snap.title, &formatted))
                } else {
                    Ok(formatted)
                }
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 12: browser_hover — Hover over element
// ═══════════════════════════════════════════════════════════════

pub struct BrowserHoverTool {
    manager: Arc<BrowserManager>,
    agent_id: String,
}

impl BrowserHoverTool {
    pub fn new(manager: Arc<BrowserManager>, agent_id: String) -> Self {
        Self { manager, agent_id }
    }
}

#[async_trait]
impl Tool for BrowserHoverTool {
    fn name(&self) -> &str {
        "browser_hover"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_hover".into(),
            description: "Hover over an element to trigger tooltips or dropdowns. \
                Use element [index] from the observation."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "index": {
                        "type": "integer",
                        "description": "Element index from observation"
                    },
                    "selector": {
                        "type": "string",
                        "description": "CSS selector (fallback)"
                    }
                }
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let session = self.manager.get_or_create(&self.agent_id).await?;
        let mut s = session.lock().await;
        s.last_active = std::time::Instant::now();

        let js = if let Some(index) = args.get("index").and_then(|v| v.as_u64()) {
            format!(
                r#"(() => {{
                    const el = document.querySelector('[data-ci="{}"]');
                    if (!el) return {{ success: false, error: 'element [{}] not found' }};
                    el.scrollIntoView({{ block: 'center' }});
                    el.dispatchEvent(new MouseEvent('mouseover', {{ bubbles: true }}));
                    el.dispatchEvent(new MouseEvent('mouseenter', {{ bubbles: true }}));
                    return {{ success: true, tag: el.tagName, text: (el.textContent||'').trim().slice(0,60) }};
                }})()"#,
                index, index
            )
        } else if let Some(selector) = args.get("selector").and_then(|v| v.as_str()) {
            let escaped = escape_js_string(selector);
            format!(
                r#"(() => {{
                    const el = document.querySelector('{}');
                    if (!el) return {{ success: false, error: 'element not found' }};
                    el.scrollIntoView({{ block: 'center' }});
                    el.dispatchEvent(new MouseEvent('mouseover', {{ bubbles: true }}));
                    el.dispatchEvent(new MouseEvent('mouseenter', {{ bubbles: true }}));
                    return {{ success: true, tag: el.tagName, text: (el.textContent||'').trim().slice(0,60) }};
                }})()"#,
                escaped
            )
        } else {
            return Err("provide either 'index' or 'selector'".to_string());
        };

        let result = s.cdp.eval(&js).await?;
        let text = result
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("");

        Ok(format!(
            "Hovered over '{}'. Call browser_observe to see any tooltips/dropdown changes.",
            text
        ))
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 13: browser_press_key — Press keyboard key
// ═══════════════════════════════════════════════════════════════

pub struct BrowserPressKeyTool {
    manager: Arc<BrowserManager>,
    agent_id: String,
}

impl BrowserPressKeyTool {
    pub fn new(manager: Arc<BrowserManager>, agent_id: String) -> Self {
        Self { manager, agent_id }
    }
}

#[async_trait]
impl Tool for BrowserPressKeyTool {
    fn name(&self) -> &str {
        "browser_press_key"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_press_key".into(),
            description: "Press a keyboard key (Enter, Tab, Escape, ArrowDown, ArrowUp, \
                Backspace, Delete, Space, etc.)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Key to press (e.g., 'Enter', 'Tab', 'Escape')"
                    }
                },
                "required": ["key"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or("missing 'key'")?;

        let session = self.manager.get_or_create(&self.agent_id).await?;
        let mut s = session.lock().await;
        s.last_active = std::time::Instant::now();

        let down_cmd = s.cdp.build_command(
            "Input.dispatchKeyEvent",
            json!({ "type": "keyDown", "key": key }),
        );
        s.cdp.send(down_cmd).await?;

        let up_cmd = s.cdp.build_command(
            "Input.dispatchKeyEvent",
            json!({ "type": "keyUp", "key": key }),
        );
        s.cdp.send(up_cmd).await?;

        // Brief pause for page reaction
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        Ok(format!("Pressed '{}' key.", key))
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 14: browser_upload — Upload file to input
// ═══════════════════════════════════════════════════════════════

pub struct BrowserUploadTool {
    manager: Arc<BrowserManager>,
    agent_id: String,
}

impl BrowserUploadTool {
    pub fn new(manager: Arc<BrowserManager>, agent_id: String) -> Self {
        Self { manager, agent_id }
    }
}

#[async_trait]
impl Tool for BrowserUploadTool {
    fn name(&self) -> &str {
        "browser_upload"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_upload".into(),
            description: "Upload a file to a <input type='file'> element on the page.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "selector": {
                        "type": "string",
                        "description": "CSS selector for the file input element"
                    },
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the file to upload"
                    }
                },
                "required": ["selector", "file_path"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser, ToolCapability::FileSystem]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let selector = args
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or("missing 'selector'")?;
        let file_path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or("missing 'file_path'")?;

        // Validate file exists
        if !std::path::Path::new(file_path).exists() {
            return Err(format!("file not found: {}", file_path));
        }

        let session = self.manager.get_or_create(&self.agent_id).await?;
        let mut s = session.lock().await;
        s.last_active = std::time::Instant::now();

        file_ops::upload_file(&s.cdp, selector, &[file_path.to_string()]).await
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 15: browser_console — Get console output
// ═══════════════════════════════════════════════════════════════

pub struct BrowserConsoleTool {
    manager: Arc<BrowserManager>,
    agent_id: String,
}

impl BrowserConsoleTool {
    pub fn new(manager: Arc<BrowserManager>, agent_id: String) -> Self {
        Self { manager, agent_id }
    }
}

#[async_trait]
impl Tool for BrowserConsoleTool {
    fn name(&self) -> &str {
        "browser_console"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_console".into(),
            description: "Get console output (console.log/warn/error) from the current page. \
                Useful for debugging JavaScript errors or checking application state."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        let session = self.manager.get_or_create(&self.agent_id).await?;
        let mut s = session.lock().await;
        s.last_active = std::time::Instant::now();

        // Ensure console shim is injected
        let _ = file_ops::inject_console_shim(&s.cdp).await;

        // Drain the buffer
        let entries = file_ops::drain_console_buffer(&s.cdp).await?;

        // Also add to the session's console log
        s.console_log.extend(entries.clone());

        Ok(file_ops::format_console_for_llm(&entries))
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 16: browser_pdf — Export page as PDF
// ═══════════════════════════════════════════════════════════════

pub struct BrowserPdfTool {
    manager: Arc<BrowserManager>,
    agent_id: String,
}

impl BrowserPdfTool {
    pub fn new(manager: Arc<BrowserManager>, agent_id: String) -> Self {
        Self { manager, agent_id }
    }
}

#[async_trait]
impl Tool for BrowserPdfTool {
    fn name(&self) -> &str {
        "browser_pdf"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browser_pdf".into(),
            description: "Export the current page as a PDF. Returns base64-encoded PDF data. \
                Only works in headless mode."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        let session = self.manager.get_or_create(&self.agent_id).await?;
        let mut s = session.lock().await;
        s.last_active = std::time::Instant::now();

        let cmd = s.cdp.build_command(
            "Page.printToPDF",
            json!({
                "printBackground": true,
                "preferCSSPageSize": true,
            }),
        );
        let resp = s.cdp.send(cmd).await?;

        let data = resp
            .result
            .as_ref()
            .and_then(|r| r.get("data"))
            .and_then(|d| d.as_str())
            .ok_or("no PDF data in response")?;

        Ok(format!("data:application/pdf;base64,{}", data))
    }
}

// ═══════════════════════════════════════════════════════════════
// Registration function
// ═══════════════════════════════════════════════════════════════

/// Register all browser tools into a ToolRegistry.
///
/// Called after `register_builtin_tools()` in the Tauri app startup.
/// Each tool captures `Arc<BrowserManager>` and the agent_id for
/// per-agent session scoping.
pub fn register_browser_tools(
    registry: &mut crate::tools::ToolRegistry,
    manager: Arc<BrowserManager>,
    agent_id: String,
) {
    registry.register(Arc::new(BrowserObserveTool::new(
        Arc::clone(&manager),
        agent_id.clone(),
    )));
    registry.register(Arc::new(BrowserNavigateTool::new(
        Arc::clone(&manager),
        agent_id.clone(),
    )));
    registry.register(Arc::new(BrowserClickTool::new(
        Arc::clone(&manager),
        agent_id.clone(),
    )));
    registry.register(Arc::new(BrowserTypeTool::new(
        Arc::clone(&manager),
        agent_id.clone(),
    )));
    registry.register(Arc::new(BrowserScreenshotTool::new(
        Arc::clone(&manager),
        agent_id.clone(),
    )));
    registry.register(Arc::new(BrowserScrollTool::new(
        Arc::clone(&manager),
        agent_id.clone(),
    )));
    registry.register(Arc::new(BrowserCloseTool::new(
        Arc::clone(&manager),
        agent_id.clone(),
    )));
    // New tools: tabs, snapshot, hover, press_key, upload, console, pdf
    registry.register(Arc::new(BrowserTabListTool::new(
        Arc::clone(&manager),
        agent_id.clone(),
    )));
    registry.register(Arc::new(BrowserTabOpenTool::new(
        Arc::clone(&manager),
        agent_id.clone(),
    )));
    registry.register(Arc::new(BrowserTabCloseTool::new(
        Arc::clone(&manager),
        agent_id.clone(),
    )));
    registry.register(Arc::new(BrowserSnapshotTool::new(
        Arc::clone(&manager),
        agent_id.clone(),
    )));
    registry.register(Arc::new(BrowserHoverTool::new(
        Arc::clone(&manager),
        agent_id.clone(),
    )));
    registry.register(Arc::new(BrowserPressKeyTool::new(
        Arc::clone(&manager),
        agent_id.clone(),
    )));
    registry.register(Arc::new(BrowserUploadTool::new(
        Arc::clone(&manager),
        agent_id.clone(),
    )));
    registry.register(Arc::new(BrowserConsoleTool::new(
        Arc::clone(&manager),
        agent_id.clone(),
    )));
    registry.register(Arc::new(BrowserPdfTool::new(
        Arc::clone(&manager),
        agent_id,
    )));
}
