//! Bundled MCP integration templates.
//!
//! Pre-packaged TOML configurations for popular MCP servers.
//! Embedded at compile time via `include_str!`.

use crate::protocol::McpServerConfig;
use crate::McpError;

/// Bundled integration template
pub struct BundledTemplate {
    pub name: &'static str,
    pub toml_content: &'static str,
    pub category: &'static str,
    pub description: &'static str,
}

/// All bundled MCP templates
pub const BUNDLED_TEMPLATES: &[BundledTemplate] = &[
    BundledTemplate {
        name: "sqlite",
        toml_content: r#"
name = "sqlite"
description = "SQLite database access via MCP"
[transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-sqlite", "--db-path", "${SQLITE_DB_PATH}"]
[env]
SQLITE_DB_PATH = ""
"#,
        category: "data",
        description: "SQLite database read/write access",
    },
    BundledTemplate {
        name: "filesystem",
        toml_content: r#"
name = "filesystem"
description = "Filesystem access via MCP"
[transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "${WORKSPACE_PATH}"]
[env]
WORKSPACE_PATH = ""
"#,
        category: "devtools",
        description: "Sandboxed filesystem operations",
    },
    BundledTemplate {
        name: "github",
        toml_content: r#"
name = "github"
description = "GitHub API access via MCP"
[transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
[env]
GITHUB_PERSONAL_ACCESS_TOKEN = ""
"#,
        category: "devtools",
        description: "GitHub issues, PRs, repos, and code search",
    },
    BundledTemplate {
        name: "brave-search",
        toml_content: r#"
name = "brave-search"
description = "Brave Search API via MCP"
[transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-brave-search"]
[env]
BRAVE_API_KEY = ""
"#,
        category: "search",
        description: "Web search via Brave Search API",
    },
    BundledTemplate {
        name: "puppeteer",
        toml_content: r#"
name = "puppeteer"
description = "Browser automation via MCP"
[transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-puppeteer"]
"#,
        category: "devtools",
        description: "Browser automation and web scraping",
    },
    BundledTemplate {
        name: "clawdesk-browser",
        toml_content: r#"
name = "clawdesk-browser"
description = "ClawDesk native browser automation via CDP with DOM Intelligence"
[transport]
type = "stdio"
command = "clawdesk"
args = ["mcp", "serve", "--tools", "browser"]
"#,
        category: "devtools",
        description: "Native CDP browser automation with actionability gates, DOM Intelligence (400-1200 tokens vs 12500 for raw HTML), and reliability layer",
    },
];

/// Get a bundled template by name.
pub fn get_template(name: &str) -> Option<&'static BundledTemplate> {
    BUNDLED_TEMPLATES.iter().find(|t| t.name == name)
}

/// List all bundled templates.
pub fn list_templates() -> &'static [BundledTemplate] {
    BUNDLED_TEMPLATES
}

/// List templates by category.
pub fn templates_by_category(category: &str) -> Vec<&'static BundledTemplate> {
    BUNDLED_TEMPLATES
        .iter()
        .filter(|t| t.category == category)
        .collect()
}

/// Parse a bundled template into an McpServerConfig.
pub fn parse_template(template: &BundledTemplate) -> Result<McpServerConfig, McpError> {
    toml::from_str(template.toml_content)
        .map_err(|e| McpError::InvalidConfig(format!("parse template '{}': {}", template.name, e)))
}

/// Available categories.
pub fn categories() -> Vec<&'static str> {
    vec!["devtools", "data", "search", "productivity", "cloud"]
}

/// Register browser automation tools on an MCP server instance.
///
/// Exposes `browser_navigate`, `browser_click`, `browser_screenshot`, `browser_observe`
/// as MCP tools. This makes `clawdesk mcp serve --tools browser` work, allowing any
/// MCP client (Claude Desktop, etc.) to use ClawDesk's browser automation.
///
/// # Arguments
/// * `server` - The MCP server to register tools on
/// * `browser_mgr` - Shared browser manager for session management
pub fn register_browser_tools(
    server: &mut crate::server::McpServer,
    browser_mgr: std::sync::Arc<clawdesk_browser::BrowserManager>,
) {
    use crate::protocol::{McpContent, McpTool, ToolCallResult};
    use crate::server::McpServerTool;
    use serde_json::json;

    // browser_navigate
    let mgr = browser_mgr.clone();
    server.register_tool(McpServerTool {
        schema: McpTool {
            name: "browser_navigate".into(),
            description: Some("Navigate to a URL and return the page observation".into()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "URL to navigate to" }
                },
                "required": ["url"]
            }),
        },
        handler: Box::new(move |args| {
            let mgr = mgr.clone();
            Box::pin(async move {
                let url = args.get("url")
                    .and_then(|v| v.as_str())
                    .ok_or("missing 'url' parameter")?;
                let session = mgr.get_or_create("mcp").await.map_err(|e| e.to_string())?;
                let mut s = session.lock().await;
                s.cdp.navigate_and_wait(url).await.map_err(|e| e.to_string())?;
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let title = s.cdp.eval("document.title").await
                    .ok()
                    .and_then(|v| v.get("result").and_then(|r| r.get("value")).and_then(|v| v.as_str().map(String::from)))
                    .unwrap_or_default();
                Ok(ToolCallResult {
                    content: vec![McpContent::Text { text: format!("Navigated to {url}. Title: {title}") }],
                    is_error: false,
                })
            })
        }),
    });

    // browser_screenshot
    let mgr = browser_mgr.clone();
    server.register_tool(McpServerTool {
        schema: McpTool {
            name: "browser_screenshot".into(),
            description: Some("Take a screenshot of the current page".into()),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        },
        handler: Box::new(move |_args| {
            let mgr = mgr.clone();
            Box::pin(async move {
                let session = mgr.get_or_create("mcp").await.map_err(|e| e.to_string())?;
                let s = session.lock().await;
                let b64 = s.cdp.take_screenshot().await.map_err(|e| e.to_string())?;
                Ok(ToolCallResult {
                    content: vec![McpContent::Image { data: b64, mime_type: "image/png".into() }],
                    is_error: false,
                })
            })
        }),
    });

    // browser_click
    let mgr = browser_mgr.clone();
    server.register_tool(McpServerTool {
        schema: McpTool {
            name: "browser_click".into(),
            description: Some("Click an element by data-ci index or CSS selector".into()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "index": { "type": "integer", "description": "Element index from observation" },
                    "selector": { "type": "string", "description": "CSS selector fallback" }
                }
            }),
        },
        handler: Box::new(move |args| {
            let mgr = mgr.clone();
            Box::pin(async move {
                let js = if let Some(index) = args.get("index").and_then(|v| v.as_u64()) {
                    clawdesk_browser::reliability::batched_click_js(
                        &format!("[data-ci='{}']", index),
                        0.1,
                    )
                } else if let Some(selector) = args.get("selector").and_then(|v| v.as_str()) {
                    clawdesk_browser::reliability::batched_click_js(selector, 0.1)
                } else {
                    return Err("provide 'index' or 'selector'".to_string());
                };
                let session = mgr.get_or_create("mcp").await.map_err(|e| e.to_string())?;
                let s = session.lock().await;
                let result = s.cdp.eval(&js).await.map_err(|e| e.to_string())?;
                let text = serde_json::to_string_pretty(&result).unwrap_or_default();
                Ok(ToolCallResult {
                    content: vec![McpContent::Text { text }],
                    is_error: false,
                })
            })
        }),
    });

    // browser_observe
    let mgr = browser_mgr.clone();
    server.register_tool(McpServerTool {
        schema: McpTool {
            name: "browser_observe".into(),
            description: Some("Get a structured DOM observation of the current page".into()),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        },
        handler: Box::new(move |_args| {
            let mgr = mgr.clone();
            Box::pin(async move {
                let session = mgr.get_or_create("mcp").await.map_err(|e| e.to_string())?;
                let s = session.lock().await;
                let snapshot = clawdesk_browser::dom_intel::extract_dom_intelligence(&s.cdp).await
                    .map_err(|e| e.to_string())?;
                Ok(ToolCallResult {
                    content: vec![McpContent::Text { text: snapshot.format_for_llm() }],
                    is_error: false,
                })
            })
        }),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_bundled_templates() {
        assert!(!BUNDLED_TEMPLATES.is_empty());
        assert!(BUNDLED_TEMPLATES.len() >= 6);
    }

    #[test]
    fn get_github_template() {
        let template = get_template("github").unwrap();
        assert_eq!(template.name, "github");
        assert_eq!(template.category, "devtools");
    }

    #[test]
    fn parse_all_templates() {
        for template in BUNDLED_TEMPLATES {
            let result = parse_template(template);
            assert!(result.is_ok(), "failed to parse template: {}", template.name);
        }
    }
}
