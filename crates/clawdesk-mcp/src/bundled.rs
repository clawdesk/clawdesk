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
