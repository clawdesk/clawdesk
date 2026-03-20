//! High-level browser actions — navigate, click, type, extract.

use serde::{Deserialize, Serialize};

/// Browser action types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BrowserAction {
    /// Navigate to URL.
    Navigate { url: String },
    /// Click an element by CSS selector.
    Click { selector: String },
    /// Type text into an input.
    Type { selector: String, text: String },
    /// Take a screenshot.
    Screenshot { format: ScreenshotFormat },
    /// Extract text from page or element.
    ExtractText { selector: Option<String> },
    /// Get page title.
    GetTitle,
    /// Get current URL.
    GetUrl,
    /// Wait for selector to appear.
    WaitForSelector { selector: String, timeout_ms: u64 },
    /// Execute arbitrary JavaScript.
    EvalJs { expression: String },
    /// Scroll page.
    Scroll { direction: ScrollDirection, amount: u32 },
    /// Go back.
    Back,
    /// Go forward.
    Forward,
    /// Reload.
    Reload,
    // ── Extended actions (Phase 3) ───────────────────────────
    /// Hover over an element.
    Hover { selector: String },
    /// Drag an element and drop on another.
    DragDrop {
        source_selector: String,
        target_selector: String,
    },
    /// Select an option from a <select> dropdown.
    SelectOption {
        selector: String,
        value: String,
    },
    /// Fill a form field using CDP Input.dispatchKeyEvent (character-by-character).
    Fill {
        selector: String,
        text: String,
    },
    /// Press a keyboard key (e.g., "Enter", "Tab", "Escape", "ArrowDown").
    PressKey { key: String },
    /// Resize the browser viewport.
    ResizeViewport { width: u32, height: u32 },
    /// Wait for navigation to complete.
    WaitForNavigation { timeout_ms: u64 },
    /// Wait for network to be idle (no requests for N ms).
    WaitForNetworkIdle { idle_ms: u64, timeout_ms: u64 },
    /// Focus an element without clicking.
    Focus { selector: String },
    /// Execute JavaScript with a safety timeout — kills execution if it exceeds timeout_ms.
    SafeEval {
        expression: String,
        timeout_ms: u64,
    },
    /// Export page as PDF (headless only).
    ExportPdf,
}

/// Screenshot format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreenshotFormat {
    Png,
    Jpeg,
    Webp,
}

impl ScreenshotFormat {
    pub fn as_cdp_format(&self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
        }
    }

    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
        }
    }
}

/// Element targeting strategy.
///
/// Index-based (preferred): Uses data-ci attribute stamped by DOM intelligence.
/// Selector-based (fallback): CSS selector for backward compatibility.
/// Coordinate-based (vision): For canvas/custom UI fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ElementTarget {
    /// Element index from DOM intelligence observation (preferred).
    Index(u32),
    /// CSS selector (fallback for backward compatibility).
    Selector(String),
    /// Screen coordinates for vision-based interaction.
    Coordinates { x: u32, y: u32 },
}

/// Scroll direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

/// Result of a browser action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    pub success: bool,
    pub action: String,
    pub data: ActionData,
    pub duration_ms: u64,
}

/// Action result data variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActionData {
    None,
    Text(String),
    Screenshot(Vec<u8>),
    Url(String),
    JsResult(serde_json::Value),
    Error(String),
}

impl ActionResult {
    pub fn ok(action: &str, data: ActionData, duration_ms: u64) -> Self {
        Self {
            success: true,
            action: action.to_string(),
            data,
            duration_ms,
        }
    }

    pub fn err(action: &str, error: String) -> Self {
        Self {
            success: false,
            action: action.to_string(),
            data: ActionData::Error(error),
            duration_ms: 0,
        }
    }
}

/// Convert browser action to tool description for LLM.
pub fn action_tool_definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "browser_navigate",
            description: "Navigate to a URL",
            parameters: vec![("url", "string", "The URL to navigate to", true)],
        },
        ToolDef {
            name: "browser_click",
            description: "Click an element by CSS selector",
            parameters: vec![("selector", "string", "CSS selector for the element", true)],
        },
        ToolDef {
            name: "browser_type",
            description: "Type text into an input element",
            parameters: vec![
                ("selector", "string", "CSS selector for the input", true),
                ("text", "string", "Text to type", true),
            ],
        },
        ToolDef {
            name: "browser_screenshot",
            description: "Take a screenshot of the current page",
            parameters: vec![],
        },
        ToolDef {
            name: "browser_extract_text",
            description: "Extract text from the page or a specific element",
            parameters: vec![("selector", "string", "Optional CSS selector", false)],
        },
        ToolDef {
            name: "browser_get_title",
            description: "Get the current page title",
            parameters: vec![],
        },
        ToolDef {
            name: "browser_eval_js",
            description: "Execute JavaScript on the page",
            parameters: vec![("expression", "string", "JavaScript to evaluate", true)],
        },
        ToolDef {
            name: "browser_hover",
            description: "Hover over an element to trigger tooltips or dropdown menus",
            parameters: vec![("selector", "string", "CSS selector for the element", true)],
        },
        ToolDef {
            name: "browser_drag_drop",
            description: "Drag an element and drop it on another",
            parameters: vec![
                ("source", "string", "CSS selector for the drag source", true),
                ("target", "string", "CSS selector for the drop target", true),
            ],
        },
        ToolDef {
            name: "browser_select",
            description: "Select an option from a <select> dropdown",
            parameters: vec![
                ("selector", "string", "CSS selector for the select element", true),
                ("value", "string", "Option value to select", true),
            ],
        },
        ToolDef {
            name: "browser_fill",
            description: "Fill a form field character by character (more realistic than type)",
            parameters: vec![
                ("selector", "string", "CSS selector for the input", true),
                ("text", "string", "Text to fill", true),
            ],
        },
        ToolDef {
            name: "browser_press_key",
            description: "Press a keyboard key (Enter, Tab, Escape, ArrowDown, etc.)",
            parameters: vec![("key", "string", "Key name to press", true)],
        },
        ToolDef {
            name: "browser_resize",
            description: "Resize the browser viewport",
            parameters: vec![
                ("width", "integer", "Viewport width in pixels", true),
                ("height", "integer", "Viewport height in pixels", true),
            ],
        },
        ToolDef {
            name: "browser_export_pdf",
            description: "Export the current page as a PDF (headless mode only)",
            parameters: vec![],
        },
    ]
}

/// Minimal tool definition for LLM integration.
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Vec<(&'static str, &'static str, &'static str, bool)>,
}

/// Parse a tool call into a browser action.
///
/// Supports both canonical names and deprecated aliases via `resolve_alias`.
/// For element-targeting tools (click, type), accepts `index` (preferred),
/// `selector` (fallback), or both — promoting `ElementTarget` through
/// the parsing layer.
pub fn parse_tool_call(name: &str, args: &serde_json::Value) -> Option<BrowserAction> {
    use crate::tool_registry::{BrowserToolId, resolve_alias};

    let tool_id = resolve_alias(name)?;

    match tool_id {
        BrowserToolId::Navigate => {
            let url = args["url"].as_str()?.to_string();
            Some(BrowserAction::Navigate { url })
        }
        BrowserToolId::Click => {
            // Prefer index-based targeting, fall back to selector.
            let selector = resolve_element_selector(args)?;
            Some(BrowserAction::Click { selector })
        }
        BrowserToolId::Type => {
            let selector = resolve_element_selector(args)?;
            let text = args["text"].as_str()?.to_string();
            Some(BrowserAction::Type { selector, text })
        }
        BrowserToolId::Screenshot => Some(BrowserAction::Screenshot {
            format: ScreenshotFormat::Png,
        }),
        BrowserToolId::ExtractText => {
            let selector = args.get("selector").and_then(|v| v.as_str()).map(String::from);
            Some(BrowserAction::ExtractText { selector })
        }
        BrowserToolId::GetTitle => Some(BrowserAction::GetTitle),
        BrowserToolId::EvalJs => {
            let expression = args["expression"].as_str()?.to_string();
            Some(BrowserAction::EvalJs { expression })
        }
        BrowserToolId::Hover => {
            let selector = resolve_element_selector(args)?;
            Some(BrowserAction::Hover { selector })
        }
        BrowserToolId::DragDrop => {
            let source = args["source"].as_str()?.to_string();
            let target = args["target"].as_str()?.to_string();
            Some(BrowserAction::DragDrop {
                source_selector: source,
                target_selector: target,
            })
        }
        BrowserToolId::Select => {
            let selector = resolve_element_selector(args)?;
            let value = args["value"].as_str()?.to_string();
            Some(BrowserAction::SelectOption { selector, value })
        }
        BrowserToolId::Fill => {
            let selector = resolve_element_selector(args)?;
            let text = args["text"].as_str()?.to_string();
            Some(BrowserAction::Fill { selector, text })
        }
        BrowserToolId::PressKey => {
            let key = args["key"].as_str()?.to_string();
            Some(BrowserAction::PressKey { key })
        }
        BrowserToolId::Resize => {
            let width = args["width"].as_u64()? as u32;
            let height = args["height"].as_u64()? as u32;
            Some(BrowserAction::ResizeViewport { width, height })
        }
        BrowserToolId::ExportPdf => Some(BrowserAction::ExportPdf),
        BrowserToolId::Scroll => {
            let dir = args.get("direction").and_then(|v| v.as_str()).unwrap_or("down");
            let direction = match dir {
                "up" => ScrollDirection::Up,
                "left" => ScrollDirection::Left,
                "right" => ScrollDirection::Right,
                _ => ScrollDirection::Down,
            };
            let amount = args.get("amount").and_then(|v| v.as_u64()).unwrap_or(3) as u32;
            Some(BrowserAction::Scroll { direction, amount })
        }
        // Tools that don't map to BrowserAction directly (handled by agent tools).
        BrowserToolId::Observe | BrowserToolId::Close |
        BrowserToolId::TabList | BrowserToolId::TabOpen | BrowserToolId::TabClose |
        BrowserToolId::Snapshot | BrowserToolId::Upload | BrowserToolId::Console => None,
    }
}

/// Resolve an element target from tool arguments.
///
/// Prefers `index` (DOM intelligence data-ci attribute) over `selector`
/// (CSS selector), converting index to `[data-ci="N"]` selector.
fn resolve_element_selector(args: &serde_json::Value) -> Option<String> {
    // Prefer index-based targeting (DOM intelligence).
    if let Some(index) = args.get("index").and_then(|v| v.as_u64()) {
        return Some(format!("[data-ci=\"{}\"]", index));
    }
    // Fall back to CSS selector.
    args.get("selector").and_then(|v| v.as_str()).map(String::from)
}

// ─── Execution Bridge ────────────────────────────────────────────────────────

/// Execute a `BrowserAction` against a live `CdpSession`.
///
/// Maps each high-level action variant to the appropriate CDP commands,
/// measures elapsed time, and returns a structured `ActionResult`.
pub async fn execute_action(
    session: &crate::cdp::CdpSession,
    action: BrowserAction,
) -> ActionResult {
    use std::time::Instant;

    let start = Instant::now();

    match action {
        BrowserAction::Navigate { url } => {
            let cmd = session.navigate(&url);
            match session.send(cmd).await {
                Ok(_) => ActionResult::ok(
                    "navigate",
                    ActionData::Url(url),
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => ActionResult::err("navigate", e),
            }
        }
        BrowserAction::Click { selector } => {
            let cmd = session.click(&selector);
            match session.send(cmd).await {
                Ok(_) => ActionResult::ok(
                    "click",
                    ActionData::None,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => ActionResult::err("click", e),
            }
        }
        BrowserAction::Type { selector, text } => {
            let cmd = session.type_text(&selector, &text);
            match session.send(cmd).await {
                Ok(_) => ActionResult::ok(
                    "type",
                    ActionData::None,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => ActionResult::err("type", e),
            }
        }
        BrowserAction::Screenshot { format: _ } => match session.take_screenshot().await {
            Ok(base64_data) => ActionResult::ok(
                "screenshot",
                ActionData::Text(base64_data),
                start.elapsed().as_millis() as u64,
            ),
            Err(e) => ActionResult::err("screenshot", e),
        },
        BrowserAction::ExtractText { selector } => {
            let js = match selector {
                Some(sel) => format!(
                    "document.querySelector('{}')?.innerText || ''",
                    sel.replace('\'', "\\'")
                ),
                None => "document.body?.innerText || ''".to_string(),
            };
            match session.eval(&js).await {
                Ok(val) => {
                    let text = val.as_str().unwrap_or("").to_string();
                    ActionResult::ok(
                        "extract_text",
                        ActionData::Text(text),
                        start.elapsed().as_millis() as u64,
                    )
                }
                Err(e) => ActionResult::err("extract_text", e),
            }
        }
        BrowserAction::GetTitle => {
            let cmd = session.get_title();
            match session.send(cmd).await {
                Ok(resp) => {
                    let title = resp
                        .result
                        .as_ref()
                        .and_then(|r| r.get("result"))
                        .and_then(|r| r.get("value"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    ActionResult::ok(
                        "get_title",
                        ActionData::Text(title),
                        start.elapsed().as_millis() as u64,
                    )
                }
                Err(e) => ActionResult::err("get_title", e),
            }
        }
        BrowserAction::GetUrl => {
            let cmd = session.get_url();
            match session.send(cmd).await {
                Ok(resp) => {
                    let url = resp
                        .result
                        .as_ref()
                        .and_then(|r| r.get("result"))
                        .and_then(|r| r.get("value"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    ActionResult::ok(
                        "get_url",
                        ActionData::Url(url),
                        start.elapsed().as_millis() as u64,
                    )
                }
                Err(e) => ActionResult::err("get_url", e),
            }
        }
        BrowserAction::WaitForSelector {
            selector,
            timeout_ms,
        } => {
            let js = format!(
                r#"new Promise((resolve, reject) => {{
                    const start = Date.now();
                    const check = () => {{
                        if (document.querySelector('{}')) return resolve(true);
                        if (Date.now() - start > {}) return reject('timeout');
                        requestAnimationFrame(check);
                    }};
                    check();
                }})"#,
                selector.replace('\'', "\\'"),
                timeout_ms
            );
            match session.eval(&js).await {
                Ok(_) => ActionResult::ok(
                    "wait_for_selector",
                    ActionData::None,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => ActionResult::err("wait_for_selector", e),
            }
        }
        BrowserAction::EvalJs { expression } => match session.eval(&expression).await {
            Ok(val) => ActionResult::ok(
                "eval_js",
                ActionData::JsResult(val),
                start.elapsed().as_millis() as u64,
            ),
            Err(e) => ActionResult::err("eval_js", e),
        },
        BrowserAction::Scroll { direction, amount } => {
            let (x, y) = match direction {
                ScrollDirection::Up => (0, -(amount as i64)),
                ScrollDirection::Down => (0, amount as i64),
                ScrollDirection::Left => (-(amount as i64), 0),
                ScrollDirection::Right => (amount as i64, 0),
            };
            let js = format!("window.scrollBy({}, {})", x, y);
            match session.eval(&js).await {
                Ok(_) => ActionResult::ok(
                    "scroll",
                    ActionData::None,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => ActionResult::err("scroll", e),
            }
        }
        BrowserAction::Back => {
            match session.eval("history.back()").await {
                Ok(_) => ActionResult::ok(
                    "back",
                    ActionData::None,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => ActionResult::err("back", e),
            }
        }
        BrowserAction::Forward => {
            match session.eval("history.forward()").await {
                Ok(_) => ActionResult::ok(
                    "forward",
                    ActionData::None,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => ActionResult::err("forward", e),
            }
        }
        BrowserAction::Reload => {
            match session.eval("location.reload()").await {
                Ok(_) => ActionResult::ok(
                    "reload",
                    ActionData::None,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => ActionResult::err("reload", e),
            }
        }

        // ── Extended Actions ─────────────────────────────────

        BrowserAction::Hover { selector } => {
            let js = format!(
                r#"(() => {{
                    const el = document.querySelector('{}');
                    if (!el) return {{ success: false, error: 'element not found' }};
                    el.scrollIntoView({{ block: 'center' }});
                    el.dispatchEvent(new MouseEvent('mouseover', {{ bubbles: true }}));
                    el.dispatchEvent(new MouseEvent('mouseenter', {{ bubbles: true }}));
                    return {{ success: true }};
                }})()"#,
                selector.replace('\'', "\\'")
            );
            match session.eval(&js).await {
                Ok(_) => ActionResult::ok(
                    "hover",
                    ActionData::None,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => ActionResult::err("hover", e),
            }
        }

        BrowserAction::DragDrop {
            source_selector,
            target_selector,
        } => {
            let js = format!(
                r#"(() => {{
                    const src = document.querySelector('{}');
                    const tgt = document.querySelector('{}');
                    if (!src) return {{ success: false, error: 'source not found' }};
                    if (!tgt) return {{ success: false, error: 'target not found' }};
                    const dt = new DataTransfer();
                    src.dispatchEvent(new DragEvent('dragstart', {{ bubbles: true, dataTransfer: dt }}));
                    tgt.dispatchEvent(new DragEvent('dragover', {{ bubbles: true, dataTransfer: dt }}));
                    tgt.dispatchEvent(new DragEvent('drop', {{ bubbles: true, dataTransfer: dt }}));
                    src.dispatchEvent(new DragEvent('dragend', {{ bubbles: true, dataTransfer: dt }}));
                    return {{ success: true }};
                }})()"#,
                source_selector.replace('\'', "\\'"),
                target_selector.replace('\'', "\\'")
            );
            match session.eval(&js).await {
                Ok(_) => ActionResult::ok(
                    "drag_drop",
                    ActionData::None,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => ActionResult::err("drag_drop", e),
            }
        }

        BrowserAction::SelectOption { selector, value } => {
            let js = format!(
                r#"(() => {{
                    const el = document.querySelector('{}');
                    if (!el || el.tagName !== 'SELECT') return {{ success: false, error: 'select element not found' }};
                    el.value = '{}';
                    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    const opt = el.options[el.selectedIndex];
                    return {{ success: true, text: opt ? opt.text : el.value }};
                }})()"#,
                selector.replace('\'', "\\'"),
                value.replace('\'', "\\'")
            );
            match session.eval(&js).await {
                Ok(val) => {
                    let text = val
                        .get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or(&value)
                        .to_string();
                    ActionResult::ok(
                        "select_option",
                        ActionData::Text(text),
                        start.elapsed().as_millis() as u64,
                    )
                }
                Err(e) => ActionResult::err("select_option", e),
            }
        }

        BrowserAction::Fill { selector, text } => {
            // Use CDP Input.dispatchKeyEvent for character-by-character typing
            // (more realistic than setting .value directly)
            let focus_js = format!(
                r#"(() => {{
                    const el = document.querySelector('{}');
                    if (!el) return {{ success: false }};
                    el.focus();
                    el.value = '';
                    return {{ success: true }};
                }})()"#,
                selector.replace('\'', "\\'")
            );
            match session.eval(&focus_js).await {
                Ok(_) => {
                    // Type each character using CDP Input domain
                    for ch in text.chars() {
                        let key_cmd = session.build_command(
                            "Input.dispatchKeyEvent",
                            serde_json::json!({
                                "type": "keyDown",
                                "text": ch.to_string(),
                                "key": ch.to_string(),
                            }),
                        );
                        if let Err(e) = session.send(key_cmd).await {
                            return ActionResult::err("fill", e);
                        }
                        let key_up = session.build_command(
                            "Input.dispatchKeyEvent",
                            serde_json::json!({
                                "type": "keyUp",
                                "key": ch.to_string(),
                            }),
                        );
                        let _ = session.send(key_up).await;
                    }
                    ActionResult::ok(
                        "fill",
                        ActionData::Text(text),
                        start.elapsed().as_millis() as u64,
                    )
                }
                Err(e) => ActionResult::err("fill", e),
            }
        }

        BrowserAction::PressKey { key } => {
            let cmd = session.build_command(
                "Input.dispatchKeyEvent",
                serde_json::json!({
                    "type": "keyDown",
                    "key": key,
                }),
            );
            match session.send(cmd).await {
                Ok(_) => {
                    let up = session.build_command(
                        "Input.dispatchKeyEvent",
                        serde_json::json!({
                            "type": "keyUp",
                            "key": key,
                        }),
                    );
                    let _ = session.send(up).await;
                    ActionResult::ok(
                        "press_key",
                        ActionData::Text(key),
                        start.elapsed().as_millis() as u64,
                    )
                }
                Err(e) => ActionResult::err("press_key", e),
            }
        }

        BrowserAction::ResizeViewport { width, height } => {
            let cmd = session.build_command(
                "Emulation.setDeviceMetricsOverride",
                serde_json::json!({
                    "width": width,
                    "height": height,
                    "deviceScaleFactor": 1,
                    "mobile": false,
                }),
            );
            match session.send(cmd).await {
                Ok(_) => ActionResult::ok(
                    "resize_viewport",
                    ActionData::Text(format!("{}x{}", width, height)),
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => ActionResult::err("resize_viewport", e),
            }
        }

        BrowserAction::WaitForNavigation { timeout_ms } => {
            let js = format!(
                r#"new Promise((resolve) => {{
                    let resolved = false;
                    const cb = () => {{ if (!resolved) {{ resolved = true; resolve(true); }} }};
                    window.addEventListener('load', cb, {{ once: true }});
                    setTimeout(() => {{ if (!resolved) {{ resolved = true; resolve(false); }} }}, {});
                }})"#,
                timeout_ms
            );
            match session.eval(&js).await {
                Ok(_) => ActionResult::ok(
                    "wait_for_navigation",
                    ActionData::None,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => ActionResult::err("wait_for_navigation", e),
            }
        }

        BrowserAction::WaitForNetworkIdle {
            idle_ms,
            timeout_ms,
        } => {
            // Use NetworkIdleTracker with hysteresis for reliable idle detection (R9 wiring).
            // The JS monitors fetch/XHR and reports idle only after continuous zero
            // pending requests for `idle_ms` milliseconds — preventing premature idle
            // detection during cascading XHR request chains.
            let js = format!(
                r#"new Promise((resolve) => {{
                    let pending = 0;
                    let timer = null;
                    const idle_ms = {};
                    const timeout_ms = {};
                    const check = () => {{
                        if (timer) clearTimeout(timer);
                        if (pending <= 0) {{
                            timer = setTimeout(() => resolve({{ idle: true, pending: 0 }}), idle_ms);
                        }}
                    }};
                    const orig_fetch = window.fetch;
                    window.fetch = function(...args) {{
                        pending++;
                        if (timer) {{ clearTimeout(timer); timer = null; }}
                        return orig_fetch.apply(this, args).finally(() => {{ pending--; check(); }});
                    }};
                    const origXHROpen = XMLHttpRequest.prototype.open;
                    const origXHRSend = XMLHttpRequest.prototype.send;
                    XMLHttpRequest.prototype.open = function(...args) {{
                        this.__clawdesk_tracked = true;
                        return origXHROpen.apply(this, args);
                    }};
                    XMLHttpRequest.prototype.send = function(...args) {{
                        if (this.__clawdesk_tracked) {{
                            pending++;
                            if (timer) {{ clearTimeout(timer); timer = null; }}
                            this.addEventListener('loadend', () => {{ pending--; check(); }}, {{ once: true }});
                        }}
                        return origXHRSend.apply(this, args);
                    }};
                    check();
                    setTimeout(() => resolve({{ idle: false, pending: pending, timeout: true }}), timeout_ms);
                }})"#,
                idle_ms, timeout_ms
            );
            match session.eval(&js).await {
                Ok(_) => ActionResult::ok(
                    "wait_for_network_idle",
                    ActionData::None,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => ActionResult::err("wait_for_network_idle", e),
            }
        }

        BrowserAction::Focus { selector } => {
            let js = format!(
                r#"(() => {{
                    const el = document.querySelector('{}');
                    if (!el) return {{ success: false, error: 'element not found' }};
                    el.scrollIntoView({{ block: 'center' }});
                    el.focus();
                    return {{ success: true }};
                }})()"#,
                selector.replace('\'', "\\'")
            );
            match session.eval(&js).await {
                Ok(_) => ActionResult::ok(
                    "focus",
                    ActionData::None,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => ActionResult::err("focus", e),
            }
        }

        BrowserAction::SafeEval {
            expression,
            timeout_ms,
        } => {
            // Use Runtime.evaluate with explicit timeout. If it exceeds,
            // call Runtime.terminateExecution to kill the script.
            let cmd = session.build_command(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                    "timeout": timeout_ms,
                }),
            );

            let timeout = std::time::Duration::from_millis(timeout_ms + 1000);
            match tokio::time::timeout(timeout, session.send(cmd)).await {
                Ok(Ok(resp)) => {
                    let val = resp.result.unwrap_or(serde_json::Value::Null);
                    ActionResult::ok(
                        "safe_eval",
                        ActionData::JsResult(val),
                        start.elapsed().as_millis() as u64,
                    )
                }
                Ok(Err(e)) => {
                    // Try to terminate runaway execution
                    let kill = session.build_command(
                        "Runtime.terminateExecution",
                        serde_json::json!({}),
                    );
                    let _ = session.send(kill).await;
                    ActionResult::err("safe_eval", e)
                }
                Err(_) => {
                    // Timed out — terminate execution
                    let kill = session.build_command(
                        "Runtime.terminateExecution",
                        serde_json::json!({}),
                    );
                    let _ = session.send(kill).await;
                    ActionResult::err(
                        "safe_eval",
                        format!("execution timed out after {}ms", timeout_ms),
                    )
                }
            }
        }

        BrowserAction::ExportPdf => {
            let cmd = session.build_command(
                "Page.printToPDF",
                serde_json::json!({
                    "printBackground": true,
                    "preferCSSPageSize": true,
                }),
            );
            match session.send(cmd).await {
                Ok(resp) => {
                    let data = resp
                        .result
                        .as_ref()
                        .and_then(|r| r.get("data"))
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string();
                    ActionResult::ok(
                        "export_pdf",
                        ActionData::Text(data),
                        start.elapsed().as_millis() as u64,
                    )
                }
                Err(e) => ActionResult::err("export_pdf", e),
            }
        }
    }
}

/// Execute a browser tool call end-to-end: parse → execute → format result.
///
/// Convenience function for integrations that receive raw tool call names
/// and JSON arguments (e.g., from an LLM tool-use response).
///
/// Returns a human-readable result string suitable for feeding back to the LLM.
pub async fn execute_tool_call(
    session: &crate::cdp::CdpSession,
    tool_name: &str,
    args: &serde_json::Value,
) -> Result<String, String> {
    let action = parse_tool_call(tool_name, args)
        .ok_or_else(|| format!("unknown browser tool: {}", tool_name))?;

    let result = execute_action(session, action).await;

    if result.success {
        match result.data {
            ActionData::None => Ok(format!("[{}] success ({}ms)", result.action, result.duration_ms)),
            ActionData::Text(t) => Ok(t),
            ActionData::Screenshot(ref _bytes) => Ok(format!(
                "[screenshot] captured ({}ms)",
                result.duration_ms
            )),
            ActionData::Url(u) => Ok(u),
            ActionData::JsResult(v) => Ok(serde_json::to_string_pretty(&v).unwrap_or_default()),
            ActionData::Error(e) => Err(e),
        }
    } else {
        match result.data {
            ActionData::Error(e) => Err(format!("[{}] failed: {}", result.action, e)),
            _ => Err(format!("[{}] failed", result.action)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_navigate() {
        let args = serde_json::json!({"url": "https://example.com"});
        let action = parse_tool_call("browser_navigate", &args).unwrap();
        match action {
            BrowserAction::Navigate { url } => assert_eq!(url, "https://example.com"),
            _ => panic!("Wrong action type"),
        }
    }

    #[test]
    fn parse_type_text() {
        let args = serde_json::json!({"selector": "#input", "text": "hello"});
        let action = parse_tool_call("browser_type", &args).unwrap();
        match action {
            BrowserAction::Type { selector, text } => {
                assert_eq!(selector, "#input");
                assert_eq!(text, "hello");
            }
            _ => panic!("Wrong action type"),
        }
    }

    #[test]
    fn parse_unknown_returns_none() {
        let args = serde_json::json!({});
        assert!(parse_tool_call("unknown_action", &args).is_none());
    }

    #[test]
    fn tool_definitions_count() {
        let defs = action_tool_definitions();
        assert_eq!(defs.len(), 14);
    }

    #[test]
    fn screenshot_format() {
        assert_eq!(ScreenshotFormat::Png.as_cdp_format(), "png");
        assert_eq!(ScreenshotFormat::Jpeg.mime_type(), "image/jpeg");
    }

    #[test]
    fn action_result_ok() {
        let r = ActionResult::ok("navigate", ActionData::Url("https://x.com".into()), 100);
        assert!(r.success);
        assert_eq!(r.duration_ms, 100);
    }
}
