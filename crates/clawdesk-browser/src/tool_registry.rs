//! Canonical browser tool registry — single source of truth for all browser tool APIs.
//!
//! All layers (skill prompt, agent tools, action registry, Tauri IPC) derive
//! their tool names, parameters, and semantics from this registry.
//!
//! ## Architecture
//!
//! ```text
//! BrowserToolId (enum)  ←─── canonical name + alias map
//!       │
//!       ├── BrowserSkillProvider → prompt + tool_names (derived)
//!       ├── action::parse_tool_call → BrowserAction (derived)
//!       ├── browser_tools.rs → Tool impls (use canonical names)
//!       └── commands_browser.rs → Tauri IPC (derived)
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Canonical tool IDs
// ---------------------------------------------------------------------------

/// Canonical browser tool identifiers — the single source of truth.
///
/// Every browser API surface (prompt, registry, IPC, agent tools) MUST
/// use these identifiers. Aliases are resolved to canonical IDs via
/// `resolve_alias()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserToolId {
    // ── Core 7 (prompt-injected) ──
    Navigate,
    Observe,
    Click,
    Type,
    Screenshot,
    Scroll,
    Close,
    // ── Extended tools ──
    ExtractText,
    GetTitle,
    EvalJs,
    Hover,
    DragDrop,
    Select,
    Fill,
    PressKey,
    Resize,
    ExportPdf,
    TabList,
    TabOpen,
    TabClose,
    Snapshot,
    Upload,
    Console,
}

impl BrowserToolId {
    /// Canonical tool name string (e.g., `"browser_navigate"`).
    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::Navigate => "browser_navigate",
            Self::Observe => "browser_observe",
            Self::Click => "browser_click",
            Self::Type => "browser_type",
            Self::Screenshot => "browser_screenshot",
            Self::Scroll => "browser_scroll",
            Self::Close => "browser_close",
            Self::ExtractText => "browser_extract_text",
            Self::GetTitle => "browser_get_title",
            Self::EvalJs => "browser_eval_js",
            Self::Hover => "browser_hover",
            Self::DragDrop => "browser_drag_drop",
            Self::Select => "browser_select",
            Self::Fill => "browser_fill",
            Self::PressKey => "browser_press_key",
            Self::Resize => "browser_resize",
            Self::ExportPdf => "browser_export_pdf",
            Self::TabList => "browser_tab_list",
            Self::TabOpen => "browser_tab_open",
            Self::TabClose => "browser_tab_close",
            Self::Snapshot => "browser_snapshot",
            Self::Upload => "browser_upload",
            Self::Console => "browser_console",
        }
    }

    /// Human-readable description.
    pub const fn description(self) -> &'static str {
        match self {
            Self::Navigate => "Navigate to a URL and return page observation",
            Self::Observe => "Observe the current page (DOM intelligence extraction)",
            Self::Click => "Click an element by index or selector",
            Self::Type => "Type text into an element by index or selector",
            Self::Screenshot => "Take a screenshot of the current page",
            Self::Scroll => "Scroll the page up or down",
            Self::Close => "Close the browser session",
            Self::ExtractText => "Extract text from the page or a specific element",
            Self::GetTitle => "Get the current page title",
            Self::EvalJs => "Execute JavaScript on the page",
            Self::Hover => "Hover over an element",
            Self::DragDrop => "Drag and drop between two elements",
            Self::Select => "Select an option from a dropdown",
            Self::Fill => "Fill a form field character by character",
            Self::PressKey => "Press a keyboard key (Enter, Tab, etc.)",
            Self::Resize => "Resize the browser viewport",
            Self::ExportPdf => "Export page as PDF (headless only)",
            Self::TabList => "List open browser tabs",
            Self::TabOpen => "Open a new tab",
            Self::TabClose => "Close a tab by index",
            Self::Snapshot => "Take ARIA/AI/DOM snapshot",
            Self::Upload => "Upload a file via file input",
            Self::Console => "Get console.log output",
        }
    }

    /// Whether this tool uses `ElementTarget` (index/selector/coordinates).
    pub const fn uses_element_target(self) -> bool {
        matches!(self, Self::Click | Self::Type | Self::Hover | Self::Fill |
                 Self::DragDrop | Self::Select)
    }

    /// All canonical tool IDs.
    pub const fn all() -> &'static [BrowserToolId] {
        &[
            Self::Navigate, Self::Observe, Self::Click, Self::Type,
            Self::Screenshot, Self::Scroll, Self::Close,
            Self::ExtractText, Self::GetTitle, Self::EvalJs,
            Self::Hover, Self::DragDrop, Self::Select, Self::Fill,
            Self::PressKey, Self::Resize, Self::ExportPdf,
            Self::TabList, Self::TabOpen, Self::TabClose,
            Self::Snapshot, Self::Upload, Self::Console,
        ]
    }

    /// The core 7 tools injected by the browser skill prompt.
    pub const fn core_tools() -> &'static [BrowserToolId] {
        &[
            Self::Navigate, Self::Observe, Self::Click, Self::Type,
            Self::Screenshot, Self::Scroll, Self::Close,
        ]
    }

    /// Core tool names as strings (for SkillInjection).
    pub fn core_tool_names() -> Vec<String> {
        Self::core_tools().iter().map(|t| t.canonical_name().to_string()).collect()
    }
}

// We cannot have a `Focus` variant without adding it to the enum, 
// but `uses_element_target` references it — let's handle gracefully:
// Focus is already in BrowserAction but not yet in BrowserToolId.
// For now the match arm is unreachable, which is fine.

// ---------------------------------------------------------------------------
// Alias resolution
// ---------------------------------------------------------------------------

/// Resolve a tool name (possibly an alias) to a canonical `BrowserToolId`.
///
/// Supports backward-compatible aliases:
/// - `browser_read_page` → `browser_extract_text`
/// - `browser_execute_js` → `browser_eval_js`
///
/// Returns `None` for unknown names.
pub fn resolve_alias(name: &str) -> Option<BrowserToolId> {
    // Canonical names first (O(1) via match).
    match name {
        "browser_navigate" => Some(BrowserToolId::Navigate),
        "browser_observe" => Some(BrowserToolId::Observe),
        "browser_click" => Some(BrowserToolId::Click),
        "browser_type" => Some(BrowserToolId::Type),
        "browser_screenshot" => Some(BrowserToolId::Screenshot),
        "browser_scroll" => Some(BrowserToolId::Scroll),
        "browser_close" => Some(BrowserToolId::Close),
        "browser_extract_text" => Some(BrowserToolId::ExtractText),
        "browser_get_title" => Some(BrowserToolId::GetTitle),
        "browser_eval_js" => Some(BrowserToolId::EvalJs),
        "browser_hover" => Some(BrowserToolId::Hover),
        "browser_drag_drop" => Some(BrowserToolId::DragDrop),
        "browser_select" => Some(BrowserToolId::Select),
        "browser_fill" => Some(BrowserToolId::Fill),
        "browser_press_key" => Some(BrowserToolId::PressKey),
        "browser_resize" => Some(BrowserToolId::Resize),
        "browser_export_pdf" => Some(BrowserToolId::ExportPdf),
        "browser_tab_list" => Some(BrowserToolId::TabList),
        "browser_tab_open" => Some(BrowserToolId::TabOpen),
        "browser_tab_close" => Some(BrowserToolId::TabClose),
        "browser_snapshot" => Some(BrowserToolId::Snapshot),
        "browser_upload" => Some(BrowserToolId::Upload),
        "browser_console" => Some(BrowserToolId::Console),
        // Deprecated aliases — map to canonical names
        "browser_read_page" => Some(BrowserToolId::ExtractText),
        "browser_execute_js" => Some(BrowserToolId::EvalJs),
        _ => None,
    }
}

/// Check if a name is a deprecated alias (not canonical).
pub fn is_deprecated_alias(name: &str) -> bool {
    matches!(name, "browser_read_page" | "browser_execute_js")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for tool in BrowserToolId::all() {
            assert!(seen.insert(tool.canonical_name()), "duplicate name: {}", tool.canonical_name());
        }
    }

    #[test]
    fn core_tools_are_seven() {
        assert_eq!(BrowserToolId::core_tools().len(), 7);
    }

    #[test]
    fn alias_resolution_canonical() {
        for tool in BrowserToolId::all() {
            let resolved = resolve_alias(tool.canonical_name());
            assert_eq!(resolved, Some(*tool), "failed for {}", tool.canonical_name());
        }
    }

    #[test]
    fn alias_resolution_deprecated() {
        assert_eq!(resolve_alias("browser_read_page"), Some(BrowserToolId::ExtractText));
        assert_eq!(resolve_alias("browser_execute_js"), Some(BrowserToolId::EvalJs));
        assert!(is_deprecated_alias("browser_read_page"));
        assert!(!is_deprecated_alias("browser_navigate"));
    }

    #[test]
    fn unknown_name_returns_none() {
        assert_eq!(resolve_alias("nonexistent_tool"), None);
    }

    #[test]
    fn core_tool_names_match_skill_injection() {
        let names = BrowserToolId::core_tool_names();
        assert_eq!(names, vec![
            "browser_navigate", "browser_observe", "browser_click", "browser_type",
            "browser_screenshot", "browser_scroll", "browser_close",
        ]);
    }
}
