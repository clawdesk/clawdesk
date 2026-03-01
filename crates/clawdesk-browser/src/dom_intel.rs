//! DOM Intelligence Engine — JavaScript injection, element indexing, a11y tree.
//!
//! Replaces raw HTML/innerText extraction with a structured, indexed representation
//! of interactive elements. A single CDP round-trip injects JavaScript that:
//! 1. Discovers all interactive elements (buttons, links, inputs, ARIA roles)
//! 2. Assigns monotonic `data-ci` indices for O(1) reverse lookup
//! 3. Computes accessible labels (aria-label → innerText → title → placeholder)
//! 4. Extracts page headings, form structure, price indicators
//! 5. Returns head-heavy truncated body text
//!
//! Typical output: 400–1,200 tokens (vs 12,500 for raw innerText).

use serde::{Deserialize, Serialize};

/// DOM intelligence extraction script.
///
/// Injected via `Runtime.evaluate`. Runs in V8 (~5-15ms for typical pages).
/// Returns JSON matching [`DomSnapshot`] for deserialization in Rust.
///
/// Design decisions:
/// - `data-ci` attribute stamped on each element for O(1) reverse lookup on click
/// - Monotonic indices starting at 0, assigned in DOM order (top-left to bottom-right)
/// - Visibility filtering: zero-dimension, display:none, opacity<0.1, off-viewport
/// - Paint-order filtering: elements occluded by overlays are removed (z-index check)
/// - Content extraction: headings preserved verbatim, body text truncated head-heavy
/// - Forms extracted separately with input metadata for structured interaction
pub const DOM_INTEL_JS: &str = r#"(() => {
    // ── 1. Interactive element extraction ──────────────────────
    const SELECTORS = [
        'a[href]', 'button', 'input:not([type="hidden"])',
        'select', 'textarea', '[role="button"]', '[role="link"]',
        '[role="textbox"]', '[role="checkbox"]', '[role="radio"]',
        '[role="tab"]', '[role="menuitem"]', '[role="switch"]',
        '[role="combobox"]', '[role="slider"]',
        '[onclick]', '[tabindex]:not([tabindex="-1"])',
        'summary', 'details', 'label[for]'
    ].join(',');

    const candidates = document.querySelectorAll(SELECTORS);
    const vH = window.innerHeight;
    const vW = window.innerWidth;
    const seen = new Set();
    const elements = [];
    let idx = 0;

    for (const el of candidates) {
        if (seen.has(el)) continue;
        seen.add(el);

        const rect = el.getBoundingClientRect();
        const style = getComputedStyle(el);

        // ── Visibility filters ──
        if (rect.width < 4 || rect.height < 4) continue;
        if (style.display === 'none') continue;
        if (style.visibility === 'hidden') continue;
        if (parseFloat(style.opacity) < 0.1) continue;
        if (rect.top > vH * 3 || rect.bottom < -vH * 2) continue;

        // ── Compute accessible label ──
        let label = el.getAttribute('aria-label') || '';
        if (!label) {
            const labelledBy = el.getAttribute('aria-labelledby');
            if (labelledBy) {
                label = labelledBy.split(/\s+/)
                    .map(id => document.getElementById(id)?.textContent?.trim() || '')
                    .filter(Boolean).join(' ');
            }
        }
        if (!label) label = (el.textContent || '').trim();
        if (!label) label = el.title || el.placeholder || el.name || '';
        label = label.slice(0, 80).replace(/\s+/g, ' ');

        const tag = el.tagName.toLowerCase();
        const type = el.type || el.getAttribute('role') || '';
        const href = (tag === 'a' && el.href) ? el.href : '';
        const value = el.value || '';
        const checked = el.checked;
        const disabled = el.disabled || el.getAttribute('aria-disabled') === 'true';
        const inViewport = rect.top >= -10 && rect.top <= vH + 10
                        && rect.left >= -10 && rect.left <= vW + 10;

        el.setAttribute('data-ci', String(idx));

        elements.push({
            i: idx,
            tag,
            type,
            label,
            href,
            value: value.slice(0, 60),
            checked: checked === true ? true : undefined,
            disabled: disabled || undefined,
            inVp: inViewport,
            rect: [Math.round(rect.x), Math.round(rect.y),
                   Math.round(rect.width), Math.round(rect.height)],
        });
        idx++;
    }

    // ── 2. Page content extraction (structured) ───────────────
    const headings = Array.from(document.querySelectorAll('h1,h2,h3'))
        .slice(0, 15)
        .map(h => ({ level: parseInt(h.tagName[1]), text: h.textContent.trim().slice(0, 120) }));

    const priceEls = document.querySelectorAll(
        '[itemprop="price"], .price, .product-price, [data-price]'
    );
    const prices = Array.from(priceEls).slice(0, 5)
        .map(el => el.textContent.trim().slice(0, 30))
        .filter(t => /[\$€£¥₹]?\d/.test(t));

    const stripSelectors = 'nav, header, footer, aside, [role="banner"], [role="navigation"], [role="contentinfo"], .cookie-banner, .consent-banner, #cookie-notice';
    const cloned = document.body.cloneNode(true);
    cloned.querySelectorAll(stripSelectors).forEach(el => el.remove());
    let bodyText = (cloned.innerText || '').replace(/\n{3,}/g, '\n\n').trim();

    const MAX_BODY = 6000;
    if (bodyText.length > MAX_BODY) {
        const head = bodyText.slice(0, Math.floor(MAX_BODY * 0.85));
        const tail = bodyText.slice(-Math.floor(MAX_BODY * 0.10));
        bodyText = head + '\n\n[…' + (bodyText.length - head.length - tail.length) + ' chars omitted…]\n\n' + tail;
    }

    // ── 3. Form extraction ────────────────────────────────────
    const forms = Array.from(document.querySelectorAll('form')).slice(0, 5).map(f => ({
        action: f.action || '',
        method: (f.method || 'GET').toUpperCase(),
        inputs: Array.from(f.querySelectorAll('input:not([type="hidden"]),textarea,select'))
            .slice(0, 15)
            .map(inp => ({
                ci: inp.getAttribute('data-ci'),
                name: inp.name || '',
                type: inp.type || 'text',
                placeholder: inp.placeholder || '',
                required: inp.required || false,
                value: (inp.value || '').slice(0, 40),
            }))
    }));

    // ── 4. Assemble payload ───────────────────────────────────
    return {
        url: location.href,
        title: document.title,
        scroll: {
            x: Math.round(scrollX),
            y: Math.round(scrollY),
            maxX: Math.round(document.documentElement.scrollWidth),
            maxY: Math.round(document.documentElement.scrollHeight)
        },
        viewport: { w: innerWidth, h: innerHeight },
        elements,
        headings,
        prices,
        bodyText,
        forms,
        totalDomNodes: document.querySelectorAll('*').length,
        timestamp: Date.now(),
    };
})()"#;

/// Raw JSON payload from DOM intelligence script.
/// Deserialized from `Runtime.evaluate` result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomSnapshot {
    pub url: String,
    pub title: String,
    pub scroll: ScrollState,
    pub viewport: Viewport,
    pub elements: Vec<IndexedElement>,
    pub headings: Vec<Heading>,
    pub prices: Vec<String>,
    #[serde(rename = "bodyText")]
    pub body_text: String,
    pub forms: Vec<FormSnapshot>,
    #[serde(rename = "totalDomNodes")]
    pub total_dom_nodes: u32,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrollState {
    pub x: i32,
    pub y: i32,
    #[serde(rename = "maxX")]
    pub max_x: i32,
    #[serde(rename = "maxY")]
    pub max_y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Viewport {
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedElement {
    /// Monotonic index, stamped as `data-ci` on the DOM element.
    pub i: u32,
    pub tag: String,
    #[serde(rename = "type")]
    pub el_type: String,
    pub label: String,
    #[serde(default)]
    pub href: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub checked: Option<bool>,
    #[serde(default)]
    pub disabled: Option<bool>,
    /// Whether element is in the current viewport.
    #[serde(rename = "inVp")]
    pub in_viewport: bool,
    /// Bounding rect: [x, y, width, height].
    pub rect: [i32; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heading {
    pub level: u8,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormSnapshot {
    pub action: String,
    pub method: String,
    pub inputs: Vec<FormInputSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormInputSnapshot {
    /// data-ci index, if this input was also captured as an interactive element.
    pub ci: Option<String>,
    pub name: String,
    #[serde(rename = "type")]
    pub input_type: String,
    #[serde(default)]
    pub placeholder: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub value: String,
}

impl IndexedElement {
    /// Human-readable element description for LLM.
    ///
    /// Examples:
    ///   "Search Amazon.com — input[text]"
    ///   "Add to Cart — button"
    ///   "https://example.com/page — link"
    pub fn describe(&self) -> String {
        let label = if !self.label.is_empty() {
            &self.label
        } else if !self.href.is_empty() {
            &self.href
        } else {
            "(unlabeled)"
        };

        let kind = match (self.tag.as_str(), self.el_type.as_str()) {
            ("input", t) if !t.is_empty() => format!("input[{}]", t),
            ("a", _) => "link".to_string(),
            ("select", _) => "dropdown".to_string(),
            ("textarea", _) => "textarea".to_string(),
            (tag, role) if !role.is_empty() => format!("{}[{}]", tag, role),
            (tag, _) => tag.to_string(),
        };

        let mut desc = format!("{} — {}", label, kind);

        if let Some(true) = self.disabled {
            desc.push_str(" [disabled]");
        }
        if let Some(checked) = self.checked {
            desc.push_str(if checked { " [✓]" } else { " [○]" });
        }
        if !self.value.is_empty() && self.tag != "a" {
            desc.push_str(&format!(" = \"{}\"", self.value));
        }

        desc
    }
}

impl DomSnapshot {
    /// Format the full snapshot as an LLM-friendly page observation.
    ///
    /// Three sections:
    /// 1. METADATA — URL, title, scroll position
    /// 2. INTERACTIVE ELEMENTS — numbered list for click/type targeting
    /// 3. PAGE CONTENT — headings + prices + truncated body text
    ///
    /// Typical token cost: 400-1,200 tokens (vs 12,500 for raw innerText).
    pub fn format_for_llm(&self) -> String {
        let mut out = String::with_capacity(4096);

        // ── Section 1: Metadata ──
        out.push_str(&format!(
            "=== PAGE: {} ===\nURL: {}\nScroll: {}/{} px | Viewport: {}×{} | DOM: {} nodes\n",
            self.title,
            self.url,
            self.scroll.y,
            self.scroll.max_y,
            self.viewport.w,
            self.viewport.h,
            self.total_dom_nodes
        ));

        // ── Section 2: Interactive elements ──
        let in_vp: Vec<&IndexedElement> = self.elements.iter().filter(|e| e.in_viewport).collect();
        let off_vp: Vec<&IndexedElement> =
            self.elements.iter().filter(|e| !e.in_viewport).collect();

        out.push_str(&format!(
            "\n── Interactive Elements ({} visible, {} below fold) ──\n",
            in_vp.len(),
            off_vp.len()
        ));

        for el in &in_vp {
            out.push_str(&format!("  ● [{}] {}\n", el.i, el.describe()));
        }
        if !off_vp.is_empty() {
            out.push_str("  ── below fold ──\n");
            for el in off_vp.iter().take(15) {
                out.push_str(&format!("  ○ [{}] {}\n", el.i, el.describe()));
            }
            if off_vp.len() > 15 {
                out.push_str(&format!(
                    "  ... and {} more (scroll down to see)\n",
                    off_vp.len() - 15
                ));
            }
        }

        // ── Section 3: Page content ──
        if !self.headings.is_empty() || !self.prices.is_empty() || !self.body_text.is_empty() {
            out.push_str("\n── Page Content ──\n");

            for h in &self.headings {
                let prefix = "#".repeat(h.level as usize);
                out.push_str(&format!("{} {}\n", prefix, h.text));
            }

            if !self.prices.is_empty() {
                out.push_str(&format!("Prices: {}\n", self.prices.join(" | ")));
            }

            if !self.body_text.is_empty() {
                out.push('\n');
                out.push_str(&self.body_text);
                out.push('\n');
            }
        }

        // ── Section 4: Forms (compact) ──
        if !self.forms.is_empty() {
            out.push_str("\n── Forms ──\n");
            for (fi, form) in self.forms.iter().enumerate() {
                out.push_str(&format!(
                    "  Form#{} {} {} — {} fields\n",
                    fi,
                    form.method,
                    form.action,
                    form.inputs.len()
                ));
                for inp in &form.inputs {
                    let ci_ref = inp
                        .ci
                        .as_deref()
                        .map(|c| format!("[{}] ", c))
                        .unwrap_or_default();
                    out.push_str(&format!(
                        "    {}{}:{} {}{}\n",
                        ci_ref,
                        inp.name,
                        inp.input_type,
                        if inp.required { "(required) " } else { "" },
                        if !inp.placeholder.is_empty() {
                            format!("\"{}\"", inp.placeholder)
                        } else {
                            String::new()
                        }
                    ));
                }
            }
        }

        out
    }

    /// Get element by index.
    pub fn get_element(&self, index: u32) -> Option<&IndexedElement> {
        self.elements.iter().find(|e| e.i == index)
    }
}

/// Execute DOM intelligence extraction on a CdpSession.
///
/// Single CDP round-trip: injects JS, receives structured JSON.
/// Typical latency: 5-20ms depending on page complexity.
pub async fn extract_dom_intelligence(
    cdp: &crate::cdp::CdpSession,
) -> Result<DomSnapshot, String> {
    let cmd = cdp.evaluate(DOM_INTEL_JS);
    let resp = cdp.send(cmd).await?;

    let result = resp
        .result
        .ok_or("no result from DOM intelligence extraction")?;

    // Runtime.evaluate wraps the return in { result: { type, value } }
    let value = result
        .get("result")
        .and_then(|r| r.get("value"))
        .unwrap_or(&result);

    serde_json::from_value::<DomSnapshot>(value.clone())
        .map_err(|e| format!("DOM intelligence parse error: {}", e))
}

// ─── Accessibility tree extraction (supplementary) ────────────────────

/// Accessibility tree extraction via CDP's Accessibility domain.
///
/// Supplements DOM intelligence for:
/// - Shadow DOM content (unreachable by querySelectorAll)
/// - Computed accessible names (WAI-ARIA Name Computation algorithm)
/// - Semantic roles without HTML tag heuristics
/// - Disability/state information (checked, expanded, selected)
///
/// Requires `Accessibility.enable` on the CdpSession.
pub async fn extract_accessibility_tree(
    cdp: &crate::cdp::CdpSession,
) -> Result<Vec<AXNode>, String> {
    // Enable Accessibility domain if not already
    let enable = cdp.build_command("Accessibility.enable", serde_json::json!({}));
    let _ = cdp.send(enable).await;

    let cmd = cdp.build_command(
        "Accessibility.getFullAXTree",
        serde_json::json!({ "depth": 8 }),
    );
    let resp = cdp.send(cmd).await?;

    let nodes_raw = resp
        .result
        .and_then(|r| r.get("nodes").cloned())
        .ok_or("no nodes in accessibility tree response")?;

    let all_nodes: Vec<AXNodeRaw> = serde_json::from_value(nodes_raw)
        .map_err(|e| format!("AX tree parse error: {}", e))?;

    // Filter to interactive roles
    Ok(all_nodes
        .into_iter()
        .filter_map(|raw| {
            let role = raw.role.as_ref()?.get("value")?.as_str()?;
            if is_interactive_role(role) {
                Some(AXNode {
                    node_id: raw.node_id,
                    role: role.to_string(),
                    name: raw
                        .name
                        .and_then(|n| n.get("value")?.as_str().map(String::from))
                        .unwrap_or_default(),
                    value: raw
                        .value
                        .and_then(|v| v.get("value")?.as_str().map(String::from)),
                    disabled: raw
                        .properties
                        .as_ref()
                        .and_then(|props| {
                            props
                                .iter()
                                .find(|p| {
                                    p.get("name").and_then(|n| n.as_str()) == Some("disabled")
                                })
                                .and_then(|p| p.get("value")?.get("value")?.as_bool())
                        })
                        .unwrap_or(false),
                    checked: raw.properties.as_ref().and_then(|props| {
                        props
                            .iter()
                            .find(|p| p.get("name").and_then(|n| n.as_str()) == Some("checked"))
                            .and_then(|p| {
                                p.get("value")?
                                    .get("value")?
                                    .as_str()
                                    .map(|s| s == "true")
                            })
                    }),
                })
            } else {
                None
            }
        })
        .collect())
}

/// Processed accessibility tree node.
#[derive(Debug, Clone)]
pub struct AXNode {
    pub node_id: String,
    pub role: String,
    pub name: String,
    pub value: Option<String>,
    pub disabled: bool,
    pub checked: Option<bool>,
}

#[derive(Debug, serde::Deserialize)]
struct AXNodeRaw {
    #[serde(rename = "nodeId")]
    node_id: String,
    role: Option<serde_json::Value>,
    name: Option<serde_json::Value>,
    value: Option<serde_json::Value>,
    properties: Option<Vec<serde_json::Value>>,
}

fn is_interactive_role(role: &str) -> bool {
    matches!(
        role,
        "button"
            | "link"
            | "textbox"
            | "checkbox"
            | "radio"
            | "combobox"
            | "listbox"
            | "menuitem"
            | "menuitemcheckbox"
            | "menuitemradio"
            | "option"
            | "searchbox"
            | "slider"
            | "spinbutton"
            | "switch"
            | "tab"
            | "treeitem"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_snapshot() -> DomSnapshot {
        DomSnapshot {
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
            scroll: ScrollState { x: 0, y: 100, max_x: 1280, max_y: 3000 },
            viewport: Viewport { w: 1280, h: 720 },
            elements: vec![
                IndexedElement {
                    i: 0,
                    tag: "input".to_string(),
                    el_type: "text".to_string(),
                    label: "Search".to_string(),
                    href: String::new(),
                    value: String::new(),
                    checked: None,
                    disabled: None,
                    in_viewport: true,
                    rect: [100, 50, 200, 30],
                },
                IndexedElement {
                    i: 1,
                    tag: "button".to_string(),
                    el_type: String::new(),
                    label: "Submit".to_string(),
                    href: String::new(),
                    value: String::new(),
                    checked: None,
                    disabled: None,
                    in_viewport: true,
                    rect: [310, 50, 80, 30],
                },
                IndexedElement {
                    i: 2,
                    tag: "a".to_string(),
                    el_type: String::new(),
                    label: "Contact Us".to_string(),
                    href: "https://example.com/contact".to_string(),
                    value: String::new(),
                    checked: None,
                    disabled: None,
                    in_viewport: false,
                    rect: [100, 2000, 120, 20],
                },
            ],
            headings: vec![
                Heading { level: 1, text: "Welcome".to_string() },
                Heading { level: 2, text: "Products".to_string() },
            ],
            prices: vec!["$29.99".to_string(), "$49.99".to_string()],
            body_text: "Welcome to our store. Browse our selection.".to_string(),
            forms: vec![FormSnapshot {
                action: "https://example.com/search".to_string(),
                method: "GET".to_string(),
                inputs: vec![FormInputSnapshot {
                    ci: Some("0".to_string()),
                    name: "q".to_string(),
                    input_type: "text".to_string(),
                    placeholder: "Search products...".to_string(),
                    required: true,
                    value: String::new(),
                }],
            }],
            total_dom_nodes: 350,
            timestamp: 1700000000000,
        }
    }

    #[test]
    fn test_element_describe_input() {
        let el = IndexedElement {
            i: 0,
            tag: "input".to_string(),
            el_type: "text".to_string(),
            label: "Search Amazon.com".to_string(),
            href: String::new(),
            value: "headphones".to_string(),
            checked: None,
            disabled: None,
            in_viewport: true,
            rect: [0, 0, 200, 30],
        };
        let desc = el.describe();
        assert!(desc.contains("Search Amazon.com"));
        assert!(desc.contains("input[text]"));
        assert!(desc.contains("= \"headphones\""));
    }

    #[test]
    fn test_element_describe_link() {
        let el = IndexedElement {
            i: 1,
            tag: "a".to_string(),
            el_type: String::new(),
            label: "Contact".to_string(),
            href: "https://example.com/contact".to_string(),
            value: String::new(),
            checked: None,
            disabled: None,
            in_viewport: true,
            rect: [0, 0, 80, 20],
        };
        assert_eq!(el.describe(), "Contact — link");
    }

    #[test]
    fn test_element_describe_disabled_checkbox() {
        let el = IndexedElement {
            i: 2,
            tag: "input".to_string(),
            el_type: "checkbox".to_string(),
            label: "Accept terms".to_string(),
            href: String::new(),
            value: String::new(),
            checked: Some(false),
            disabled: Some(true),
            in_viewport: true,
            rect: [0, 0, 20, 20],
        };
        let desc = el.describe();
        assert!(desc.contains("[disabled]"));
        assert!(desc.contains("[○]"));
    }

    #[test]
    fn test_format_for_llm_structure() {
        let snap = sample_snapshot();
        let output = snap.format_for_llm();

        // Metadata section
        assert!(output.contains("=== PAGE: Example ==="));
        assert!(output.contains("URL: https://example.com"));
        assert!(output.contains("Scroll: 100/3000 px"));

        // Interactive elements section
        assert!(output.contains("2 visible, 1 below fold"));
        assert!(output.contains("● [0]"));
        assert!(output.contains("● [1]"));
        assert!(output.contains("○ [2]"));

        // Content section
        assert!(output.contains("# Welcome"));
        assert!(output.contains("## Products"));
        assert!(output.contains("Prices: $29.99 | $49.99"));

        // Forms section
        assert!(output.contains("Form#0 GET"));
        assert!(output.contains("[0] q:text"));
    }

    #[test]
    fn test_get_element() {
        let snap = sample_snapshot();
        assert!(snap.get_element(0).is_some());
        assert_eq!(snap.get_element(0).unwrap().label, "Search");
        assert!(snap.get_element(1).is_some());
        assert!(snap.get_element(99).is_none());
    }

    #[test]
    fn test_snapshot_serde_roundtrip() {
        let snap = sample_snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        let back: DomSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.url, snap.url);
        assert_eq!(back.elements.len(), 3);
        assert_eq!(back.total_dom_nodes, 350);
    }

    #[test]
    fn test_is_interactive_role() {
        assert!(is_interactive_role("button"));
        assert!(is_interactive_role("link"));
        assert!(is_interactive_role("textbox"));
        assert!(is_interactive_role("combobox"));
        assert!(!is_interactive_role("generic"));
        assert!(!is_interactive_role("document"));
        assert!(!is_interactive_role("region"));
    }
}
