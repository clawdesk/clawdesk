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
    ]
}

/// Minimal tool definition for LLM integration.
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Vec<(&'static str, &'static str, &'static str, bool)>,
}

/// Parse a tool call into a browser action.
pub fn parse_tool_call(name: &str, args: &serde_json::Value) -> Option<BrowserAction> {
    match name {
        "browser_navigate" => {
            let url = args["url"].as_str()?.to_string();
            Some(BrowserAction::Navigate { url })
        }
        "browser_click" => {
            let selector = args["selector"].as_str()?.to_string();
            Some(BrowserAction::Click { selector })
        }
        "browser_type" => {
            let selector = args["selector"].as_str()?.to_string();
            let text = args["text"].as_str()?.to_string();
            Some(BrowserAction::Type { selector, text })
        }
        "browser_screenshot" => Some(BrowserAction::Screenshot {
            format: ScreenshotFormat::Png,
        }),
        "browser_extract_text" => {
            let selector = args.get("selector").and_then(|v| v.as_str()).map(String::from);
            Some(BrowserAction::ExtractText { selector })
        }
        "browser_get_title" => Some(BrowserAction::GetTitle),
        "browser_eval_js" => {
            let expression = args["expression"].as_str()?.to_string();
            Some(BrowserAction::EvalJs { expression })
        }
        _ => None,
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
        assert_eq!(defs.len(), 7);
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
