//! Declarative tool manifest — TOML-based tool definitions loaded at runtime.
//!
//! Tools are described as TOML files in `tools/bundled/` (or custom directories),
//! enabling tool authoring without Rust compilation.
//!
//! ## Tool Types
//!
//! ```text
//! Tool = NativeTool | WasmTool | ScriptTool | HttpTool | McpTool
//! ```
//!
//! - **NativeTool**: Compiled Rust tool, referenced by function name
//! - **WasmTool**: WebAssembly module loaded at runtime
//! - **ScriptTool**: External script (Python, Node.js, shell)
//! - **HttpTool**: HTTP endpoint that implements the tool interface
//! - **McpTool**: Tool exposed via an MCP server
//!
//! ## Manifest Format
//!
//! ```toml
//! [tool]
//! name = "web_search"
//! description = "Search the web using a search engine API"
//! version = "1.0.0"
//! type = "http"
//!
//! [tool.parameters]
//! query = { type = "string", description = "Search query", required = true }
//! num_results = { type = "integer", description = "Number of results", default = 10 }
//!
//! [execution]
//! endpoint = "https://api.example.com/search"
//! method = "POST"
//! timeout_secs = 30
//!
//! [security]
//! requires = ["network"]
//! isolation = "none"
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

// ─────────────────────────────────────────────────────────────────────────────
// Tool manifest types
// ─────────────────────────────────────────────────────────────────────────────

/// Top-level tool manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolManifest {
    pub tool: ToolDefinition,
    #[serde(default)]
    pub execution: ExecutionConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

/// Tool identity and parameter schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(rename = "type")]
    pub tool_type: ToolType,
    #[serde(default)]
    pub parameters: HashMap<String, ParameterDef>,
    /// Tags for discovery and filtering.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Agents that can use this tool (empty = all).
    #[serde(default)]
    pub allowed_agents: Vec<String>,
}

fn default_version() -> String {
    "1.0.0".into()
}

/// Tool implementation type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolType {
    Native,
    Wasm,
    Script,
    Http,
    Mcp,
}

/// Parameter definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterDef {
    #[serde(rename = "type")]
    pub param_type: String,
    pub description: String,
    #[serde(default)]
    pub required: bool,
    pub default: Option<serde_json::Value>,
    /// Allowed values (enum constraint).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_values: Vec<serde_json::Value>,
}

/// How the tool is executed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionConfig {
    /// For HTTP tools: endpoint URL.
    pub endpoint: Option<String>,
    /// For HTTP tools: HTTP method.
    pub method: Option<String>,
    /// For script tools: command to run.
    pub command: Option<String>,
    /// For script tools: command arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// For Wasm tools: path to the .wasm module.
    pub wasm_path: Option<String>,
    /// For Wasm tools: exported function name.
    pub wasm_function: Option<String>,
    /// For MCP tools: MCP server name.
    pub mcp_server: Option<String>,
    /// Timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Environment variables for script execution.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Working directory for script execution.
    pub working_dir: Option<String>,
}

fn default_timeout() -> u64 {
    30
}

/// Security constraints for the tool.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Required capabilities (e.g., ["network", "filesystem"]).
    #[serde(default)]
    pub requires: Vec<String>,
    /// Isolation level: "none", "path_scope", "process", "full_sandbox".
    #[serde(default = "default_isolation")]
    pub isolation: String,
    /// Whether user approval is required before execution.
    #[serde(default)]
    pub requires_approval: bool,
    /// Risk level: "low", "medium", "high", "critical".
    #[serde(default = "default_risk")]
    pub risk_level: String,
}

fn default_isolation() -> String {
    "none".into()
}

fn default_risk() -> String {
    "low".into()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tool pack types
// ─────────────────────────────────────────────────────────────────────────────

/// A tool pack groups related tools with shared metadata and dependencies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPack {
    pub pack: PackInfo,
    pub tools: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

/// Pack identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackInfo {
    pub name: String,
    pub description: String,
    #[serde(default = "default_version")]
    pub version: String,
    pub author: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Loader
// ─────────────────────────────────────────────────────────────────────────────

/// Load tool manifests from a directory.
pub fn load_tool_dir(dir: &Path) -> Result<Vec<ToolManifest>, String> {
    if !dir.is_dir() {
        return Err(format!("not a directory: {}", dir.display()));
    }

    let mut manifests = Vec::new();
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("failed to read directory: {e}"))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            match load_tool_file(&path) {
                Ok(manifest) => {
                    debug!(
                        tool = %manifest.tool.name,
                        tool_type = ?manifest.tool.tool_type,
                        "loaded tool manifest"
                    );
                    manifests.push(manifest);
                }
                Err(e) => warn!(path = %path.display(), error = %e, "failed to load tool manifest"),
            }
        }
    }

    info!(count = manifests.len(), dir = %dir.display(), "loaded tool manifests");
    Ok(manifests)
}

/// Load a single tool manifest TOML file.
pub fn load_tool_file(path: &Path) -> Result<ToolManifest, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("read failed: {e}"))?;

    let manifest: ToolManifest = toml::from_str(&content)
        .map_err(|e| format!("TOML parse error: {e}"))?;

    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// Validate a tool manifest for consistency.
fn validate_manifest(manifest: &ToolManifest) -> Result<(), String> {
    if manifest.tool.name.is_empty() {
        return Err("tool name cannot be empty".into());
    }
    if manifest.tool.name.contains(char::is_whitespace) {
        return Err("tool name cannot contain whitespace".into());
    }
    if manifest.tool.description.is_empty() {
        return Err("tool description cannot be empty".into());
    }

    // Type-specific validation
    match manifest.tool.tool_type {
        ToolType::Http => {
            if manifest.execution.endpoint.is_none() {
                return Err("HTTP tool requires execution.endpoint".into());
            }
        }
        ToolType::Script => {
            if manifest.execution.command.is_none() {
                return Err("Script tool requires execution.command".into());
            }
        }
        ToolType::Wasm => {
            if manifest.execution.wasm_path.is_none() {
                return Err("Wasm tool requires execution.wasm_path".into());
            }
        }
        ToolType::Mcp => {
            if manifest.execution.mcp_server.is_none() {
                return Err("MCP tool requires execution.mcp_server".into());
            }
        }
        ToolType::Native => {}
    }

    Ok(())
}

/// Convert a tool manifest to a JSON Schema for LLM function calling.
pub fn manifest_to_json_schema(manifest: &ToolManifest) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for (name, param) in &manifest.tool.parameters {
        let mut prop = serde_json::Map::new();
        prop.insert("type".into(), serde_json::Value::String(param.param_type.clone()));
        prop.insert("description".into(), serde_json::Value::String(param.description.clone()));

        if let Some(default) = &param.default {
            prop.insert("default".into(), default.clone());
        }
        if !param.allowed_values.is_empty() {
            prop.insert("enum".into(), serde_json::Value::Array(param.allowed_values.clone()));
        }

        properties.insert(name.clone(), serde_json::Value::Object(prop));

        if param.required {
            required.push(serde_json::Value::String(name.clone()));
        }
    }

    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_http_tool() {
        let toml_str = r#"
[tool]
name = "web_search"
description = "Search the web"
type = "http"

[tool.parameters.query]
type = "string"
description = "Search query"
required = true

[tool.parameters.num_results]
type = "integer"
description = "Number of results"
default = 10

[execution]
endpoint = "https://api.example.com/search"
method = "POST"
timeout_secs = 30

[security]
requires = ["network"]
"#;
        let manifest: ToolManifest = toml::from_str(toml_str).expect("parse");
        assert_eq!(manifest.tool.name, "web_search");
        assert_eq!(manifest.tool.tool_type, ToolType::Http);
        assert_eq!(manifest.tool.parameters.len(), 2);
        assert!(manifest.tool.parameters["query"].required);
        assert_eq!(manifest.execution.endpoint.as_deref(), Some("https://api.example.com/search"));
        validate_manifest(&manifest).expect("valid");
    }

    #[test]
    fn parse_script_tool() {
        let toml_str = r#"
[tool]
name = "python_eval"
description = "Evaluate Python code"
type = "script"

[tool.parameters.code]
type = "string"
description = "Python code to evaluate"
required = true

[execution]
command = "python3"
args = ["-c"]
timeout_secs = 60

[security]
requires = ["filesystem"]
isolation = "process"
risk_level = "high"
requires_approval = true
"#;
        let manifest: ToolManifest = toml::from_str(toml_str).expect("parse");
        assert_eq!(manifest.tool.tool_type, ToolType::Script);
        assert_eq!(manifest.execution.command.as_deref(), Some("python3"));
        assert!(manifest.security.requires_approval);
    }

    #[test]
    fn validate_empty_name_fails() {
        let manifest = ToolManifest {
            tool: ToolDefinition {
                name: String::new(),
                description: "test".into(),
                version: "1.0.0".into(),
                tool_type: ToolType::Native,
                parameters: HashMap::new(),
                tags: vec![],
                allowed_agents: vec![],
            },
            execution: ExecutionConfig::default(),
            security: SecurityConfig::default(),
            metadata: HashMap::new(),
        };
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn json_schema_generation() {
        let manifest = ToolManifest {
            tool: ToolDefinition {
                name: "test_tool".into(),
                description: "A test tool".into(),
                version: "1.0.0".into(),
                tool_type: ToolType::Native,
                parameters: {
                    let mut p = HashMap::new();
                    p.insert("query".into(), ParameterDef {
                        param_type: "string".into(),
                        description: "Search query".into(),
                        required: true,
                        default: None,
                        allowed_values: vec![],
                    });
                    p
                },
                tags: vec![],
                allowed_agents: vec![],
            },
            execution: ExecutionConfig::default(),
            security: SecurityConfig::default(),
            metadata: HashMap::new(),
        };

        let schema = manifest_to_json_schema(&manifest);
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
        assert_eq!(schema["required"][0], "query");
    }
}
