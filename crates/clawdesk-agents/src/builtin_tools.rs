//! Concrete tool implementations — shell, HTTP, file I/O, memory.
//!
//! Skills teach the LLM which tools to call via prompt instructions.
//! All execution flows through these builtin tools — no stub handlers.
//! Each tool implements the `Tool` trait for dispatch via the `ToolRegistry`.

use crate::tools::{Tool, ToolCapability, ToolSchema};
use async_trait::async_trait;
use serde::{Serialize, Deserialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, info, warn};

// ─── Background Process Registry ─────────────────────────────────────────────

/// Entry in the background process registry.
#[derive(Debug)]
struct BgProcess {
    child: tokio::sync::Mutex<Option<tokio::process::Child>>,
    stdout_buf: tokio::sync::Mutex<String>,
    stderr_buf: tokio::sync::Mutex<String>,
    started_at: std::time::Instant,
    command: String,
    exit_code: tokio::sync::Mutex<Option<i32>>,
}

/// Global registry of background processes keyed by session ID.
/// The LLM can poll, send input, or kill background processes.
static BG_PROCESSES: std::sync::LazyLock<
    tokio::sync::RwLock<std::collections::HashMap<String, Arc<BgProcess>>>,
> = std::sync::LazyLock::new(|| tokio::sync::RwLock::new(std::collections::HashMap::new()));

// ─── Shell Execution Tool ────────────────────────────────────────────────────

/// Executes shell commands via `tokio::process::Command`.
///
/// Supports:
/// - Workspace-scoped execution (CWD confinement)
/// - Background execution (returns immediately with session ID)
/// - Graceful kill (SIGTERM → grace period → SIGKILL)
/// - Configurable timeout with exit code reporting
/// - Environment variable passthrough
/// - Output truncation to prevent memory exhaustion
///
/// Aligns with OpenClaw's `exec` tool: skills reference `shell_exec` in their
/// prompt instructions, and the LLM calls this tool to execute the commands.
pub struct ShellTool {
    /// If set, commands are executed relative to this directory.
    workspace: Option<PathBuf>,
    /// Maximum output size in bytes to prevent memory exhaustion.
    max_output_bytes: usize,
    /// Default timeout in seconds (overridable per-call).
    default_timeout_secs: u64,
}

impl ShellTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self {
            workspace,
            max_output_bytes: 256 * 1024, // 256 KB
            default_timeout_secs: 120,     // 2 minutes
        }
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell_exec"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "shell_exec".into(),
            description: "Execute a shell command and return stdout/stderr. Use for running scripts, CLI tools, checking system state, or performing computations. Skills that reference CLI commands (e.g. `memo notes`, `osascript`, `bear`) should be executed through this tool.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Optional working directory (relative to workspace)"
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Timeout in seconds (default: 120). Use higher values for long-running commands."
                    },
                    "background": {
                        "type": "boolean",
                        "description": "If true, run in background and return a session_id for polling. Use for long-running commands (builds, servers, data processing)."
                    },
                    "env": {
                        "type": "object",
                        "description": "Optional environment variables to set for this command",
                        "additionalProperties": { "type": "string" }
                    }
                },
                "required": ["command"]
            }),
        }
    }

    fn is_blocking(&self) -> bool {
        false // Background mode returns immediately
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ShellExec]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or("missing 'command' argument")?;

        let background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.default_timeout_secs);

        let working_dir = args
            .get("working_dir")
            .and_then(|v| v.as_str())
            .map(|d| {
                if let Some(ref ws) = self.workspace {
                    ws.join(d)
                } else {
                    PathBuf::from(d)
                }
            })
            .or_else(|| self.workspace.clone());

        // Parse optional env vars
        let env_vars: Vec<(String, String)> = args
            .get("env")
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        debug!(command, ?working_dir, background, timeout_secs, "ShellTool executing");

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(command);
        if let Some(ref dir) = working_dir {
            cmd.current_dir(dir);
        }
        for (key, val) in &env_vars {
            cmd.env(key, val);
        }
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        // ── Background execution ──
        if background {
            let session_id = format!("bg_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("x"));
            let child = cmd.spawn()
                .map_err(|e| format!("failed to spawn background process: {e}"))?;
            let entry = Arc::new(BgProcess {
                child: tokio::sync::Mutex::new(Some(child)),
                stdout_buf: tokio::sync::Mutex::new(String::new()),
                stderr_buf: tokio::sync::Mutex::new(String::new()),
                started_at: std::time::Instant::now(),
                command: command.to_string(),
                exit_code: tokio::sync::Mutex::new(None),
            });
            BG_PROCESSES.write().await.insert(session_id.clone(), Arc::clone(&entry));

            // Spawn a reader task to collect output
            let entry_clone = Arc::clone(&entry);
            let sid = session_id.clone();
            tokio::spawn(async move {
                let mut child_opt = entry_clone.child.lock().await;
                if let Some(ref mut child) = *child_opt {
                    let status = child.wait().await;
                    let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
                    *entry_clone.exit_code.lock().await = Some(code);
                    // Read any remaining output
                    if let Some(ref mut stdout) = child.stdout {
                        use tokio::io::AsyncReadExt;
                        let mut buf = Vec::new();
                        let _ = stdout.read_to_end(&mut buf).await;
                        let mut out = entry_clone.stdout_buf.lock().await;
                        out.push_str(&String::from_utf8_lossy(&buf));
                    }
                    if let Some(ref mut stderr) = child.stderr {
                        use tokio::io::AsyncReadExt;
                        let mut buf = Vec::new();
                        let _ = stderr.read_to_end(&mut buf).await;
                        let mut err = entry_clone.stderr_buf.lock().await;
                        err.push_str(&String::from_utf8_lossy(&buf));
                    }
                    info!(session = %sid, code, "background process completed");
                }
            });

            return Ok(format!(
                "Background process started.\nSession ID: {}\nCommand: {}\n\nUse shell_exec with command \"bg_status {}\" to check progress, or \"bg_kill {}\" to stop it.",
                session_id, command, session_id, session_id
            ));
        }

        // ── Check for background process control commands ──
        if let Some(stripped) = command.strip_prefix("bg_status ") {
            let sid = stripped.trim();
            let procs = BG_PROCESSES.read().await;
            if let Some(entry) = procs.get(sid) {
                let code = entry.exit_code.lock().await;
                let elapsed = entry.started_at.elapsed().as_secs();
                let stdout = entry.stdout_buf.lock().await;
                let stderr = entry.stderr_buf.lock().await;
                let status = if code.is_some() { "completed" } else { "running" };
                return Ok(format!(
                    "Session: {sid}\nStatus: {status}\nElapsed: {elapsed}s\nExit code: {}\nStdout ({} bytes):\n{}\nStderr ({} bytes):\n{}",
                    code.map(|c| c.to_string()).unwrap_or_else(|| "N/A".into()),
                    stdout.len(), truncate_output(&stdout, self.max_output_bytes),
                    stderr.len(), truncate_output(&stderr, self.max_output_bytes),
                ));
            }
            return Err(format!("No background process with session ID '{sid}'"));
        }

        if let Some(stripped) = command.strip_prefix("bg_kill ") {
            let sid = stripped.trim();
            let procs = BG_PROCESSES.read().await;
            if let Some(entry) = procs.get(sid) {
                let mut child_opt = entry.child.lock().await;
                if let Some(ref mut child) = *child_opt {
                    // Graceful kill: SIGTERM → 5s grace → SIGKILL
                    #[cfg(unix)]
                    {
                        if let Some(pid) = child.id() {
                            // Send SIGTERM via libc
                            unsafe { libc::kill(pid as i32, libc::SIGTERM); }
                        }
                    }
                    tokio::select! {
                        _ = child.wait() => {}
                        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                            let _ = child.kill().await;
                        }
                    }
                    return Ok(format!("Background process '{sid}' terminated."));
                }
                return Ok(format!("Background process '{sid}' already exited."));
            }
            return Err(format!("No background process with session ID '{sid}'"));
        }

        // ── Foreground execution with timeout ──
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            cmd.output(),
        )
        .await
        .map_err(|_| format!("command timed out after {timeout_secs}s — consider using background:true for long-running commands"))?
        .map_err(|e| format!("failed to execute command: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut result = String::new();
        if !stdout.is_empty() {
            result.push_str(&truncate_output(&stdout, self.max_output_bytes));
        }
        if !stderr.is_empty() {
            if !result.is_empty() {
                result.push_str("\n--- stderr ---\n");
            }
            result.push_str(&truncate_output(&stderr, self.max_output_bytes));
        }

        if !output.status.success() {
            result.push_str(&format!("\n[exit code: {}]", output.status.code().unwrap_or(-1)));
        }

        if result.is_empty() {
            result = format!("[command completed with exit code {}]", output.status.code().unwrap_or(0));
        }

        Ok(result)
    }
}

/// Truncate output to max_bytes, preserving UTF-8 boundaries.
fn truncate_output(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Find a valid UTF-8 boundary near max_bytes
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// ─── HTTP Fetch Tool ─────────────────────────────────────────────────────────

/// Makes HTTP requests via `reqwest`. Supports GET and POST.
pub struct HttpTool {
    client: reqwest::Client,
    max_response_bytes: usize,
}

impl HttpTool {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("ClawDesk/1.0")
            .build()
            .unwrap_or_default();
        Self {
            client,
            max_response_bytes: 512 * 1024, // 512 KB
        }
    }
}

#[async_trait]
impl Tool for HttpTool {
    fn name(&self) -> &str {
        "http_fetch"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "http_fetch".into(),
            description: "Make an HTTP request to a URL. Returns the response body. Useful for web searches, API calls, and fetching web content.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch"
                    },
                    "method": {
                        "type": "string",
                        "enum": ["GET", "POST", "PUT", "DELETE"],
                        "description": "HTTP method (default: GET)"
                    },
                    "body": {
                        "type": "string",
                        "description": "Request body (for POST/PUT)"
                    },
                    "headers": {
                        "type": "object",
                        "description": "Additional HTTP headers as key-value pairs"
                    }
                },
                "required": ["url"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or("missing 'url' argument")?;

        let method = args
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET")
            .to_uppercase();

        debug!(url, method, "HttpTool fetching");

        let mut request = match method.as_str() {
            "POST" => self.client.post(url),
            "PUT" => self.client.put(url),
            "DELETE" => self.client.delete(url),
            _ => self.client.get(url),
        };

        // Add custom headers
        if let Some(headers) = args.get("headers").and_then(|v| v.as_object()) {
            for (key, value) in headers {
                if let Some(val) = value.as_str() {
                    request = request.header(key.as_str(), val);
                }
            }
        }

        // Add body
        if let Some(body) = args.get("body").and_then(|v| v.as_str()) {
            request = request.body(body.to_string());
        }

        let response = request
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        let status = response.status();
        let headers = response.headers().clone();
        let content_type = headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        let body = response
            .text()
            .await
            .map_err(|e| format!("failed to read response body: {e}"))?;

        let truncated = if body.len() > self.max_response_bytes {
            format!(
                "{}...\n[truncated: {} bytes total]",
                &body[..self.max_response_bytes],
                body.len()
            )
        } else {
            body
        };

        Ok(format!(
            "HTTP {} {}\nContent-Type: {}\n\n{}",
            status.as_u16(),
            status.canonical_reason().unwrap_or(""),
            content_type,
            truncated
        ))
    }
}

// ─── File Read Tool ──────────────────────────────────────────────────────────

/// Reads file contents. Scoped to an optional workspace directory.
pub struct FileReadTool {
    workspace: Option<PathBuf>,
    max_bytes: usize,
}

impl FileReadTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self {
            workspace,
            max_bytes: 512 * 1024, // 512 KB
        }
    }

    fn resolve_path(&self, path: &str) -> Result<PathBuf, String> {
        let target = Path::new(path);
        if let Some(ref ws) = self.workspace {
            let resolved = if target.is_absolute() {
                target.to_path_buf()
            } else {
                ws.join(target)
            };
            // Security: ensure resolved path is within workspace
            let canonical = resolved
                .canonicalize()
                .map_err(|e| format!("path resolution failed: {e}"))?;
            let ws_canonical = ws
                .canonicalize()
                .map_err(|e| format!("workspace path resolution failed: {e}"))?;
            if !canonical.starts_with(&ws_canonical) {
                return Err("path escapes workspace boundary".to_string());
            }
            Ok(canonical)
        } else {
            Ok(target.to_path_buf())
        }
    }
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "file_read".into(),
            description: "Read the contents of a file. Returns the text content. Use for examining source code, configuration files, or documents.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read (relative to workspace or absolute)"
                    },
                    "start_line": {
                        "type": "integer",
                        "description": "Optional start line (1-based) for partial read"
                    },
                    "end_line": {
                        "type": "integer",
                        "description": "Optional end line (1-based, inclusive) for partial read"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn is_blocking(&self) -> bool {
        true
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::FileSystem]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("missing 'path' argument")?;

        let resolved = self.resolve_path(path_str)?;
        debug!(?resolved, "FileReadTool reading");

        let content = tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| format!("failed to read file: {e}"))?;

        let start_line = args.get("start_line").and_then(|v| v.as_u64()).map(|v| v as usize);
        let end_line = args.get("end_line").and_then(|v| v.as_u64()).map(|v| v as usize);

        let result = if start_line.is_some() || end_line.is_some() {
            let lines: Vec<&str> = content.lines().collect();
            let start = start_line.unwrap_or(1).saturating_sub(1);
            let end = end_line.unwrap_or(lines.len()).min(lines.len());
            lines[start..end].join("\n")
        } else if content.len() > self.max_bytes {
            format!(
                "{}...\n[truncated: {} bytes total, {} lines]",
                &content[..self.max_bytes],
                content.len(),
                content.lines().count()
            )
        } else {
            content
        };

        Ok(result)
    }
}

// ─── File Write Tool ─────────────────────────────────────────────────────────

/// Writes content to a file. Scoped to an optional workspace directory.
pub struct FileWriteTool {
    workspace: Option<PathBuf>,
}

impl FileWriteTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self { workspace }
    }

    fn resolve_path(&self, path: &str) -> Result<PathBuf, String> {
        let target = Path::new(path);
        if let Some(ref ws) = self.workspace {
            let resolved = if target.is_absolute() {
                target.to_path_buf()
            } else {
                ws.join(target)
            };
            // For writes, we can't canonicalize non-existent paths, so check parent
            if let Some(parent) = resolved.parent() {
                if parent.exists() {
                    let parent_canonical = parent
                        .canonicalize()
                        .map_err(|e| format!("path resolution failed: {e}"))?;
                    let ws_canonical = ws
                        .canonicalize()
                        .map_err(|e| format!("workspace path resolution failed: {e}"))?;
                    if !parent_canonical.starts_with(&ws_canonical) {
                        return Err("path escapes workspace boundary".to_string());
                    }
                }
            }
            Ok(resolved)
        } else {
            Ok(target.to_path_buf())
        }
    }
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "file_write".into(),
            description: "Write content to a file. Creates the file if it doesn't exist, or overwrites it. Use for saving results, creating scripts, or modifying configuration.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write (relative to workspace or absolute)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    },
                    "append": {
                        "type": "boolean",
                        "description": "If true, append to file instead of overwriting (default: false)"
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    fn is_blocking(&self) -> bool {
        true
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::FileSystem]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("missing 'path' argument")?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("missing 'content' argument")?;

        let append = args
            .get("append")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let resolved = self.resolve_path(path_str)?;
        debug!(?resolved, append, "FileWriteTool writing");

        // Create parent directories if needed
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("failed to create directories: {e}"))?;
        }

        if append {
            use tokio::io::AsyncWriteExt;
            let mut file = tokio::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&resolved)
                .await
                .map_err(|e| format!("failed to open file for append: {e}"))?;
            file.write_all(content.as_bytes())
                .await
                .map_err(|e| format!("failed to append: {e}"))?;
        } else {
            tokio::fs::write(&resolved, content)
                .await
                .map_err(|e| format!("failed to write file: {e}"))?;
        }

        Ok(format!(
            "Successfully {} {} bytes to {}",
            if append { "appended" } else { "wrote" },
            content.len(),
            path_str
        ))
    }
}

// ─── File List Tool ──────────────────────────────────────────────────────────

/// Lists directory contents. Scoped to an optional workspace directory.
pub struct FileListTool {
    workspace: Option<PathBuf>,
}

impl FileListTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for FileListTool {
    fn name(&self) -> &str {
        "file_list"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "file_list".into(),
            description: "List files and directories at a given path. Returns names, types (file/dir), and sizes.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path to list (relative to workspace or absolute). Default: workspace root."
                    },
                    "recursive": {
                        "type": "boolean",
                        "description": "If true, list recursively (default: false)"
                    }
                }
            }),
        }
    }

    fn is_blocking(&self) -> bool {
        true
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::FileSystem]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        let target = if let Some(ref ws) = self.workspace {
            ws.join(path_str)
        } else {
            PathBuf::from(path_str)
        };

        let mut entries = Vec::new();
        let mut dir = tokio::fs::read_dir(&target)
            .await
            .map_err(|e| format!("failed to read directory: {e}"))?;

        while let Ok(Some(entry)) = dir.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            let metadata = entry.metadata().await;
            let (ftype, size) = match metadata {
                Ok(m) => {
                    let t = if m.is_dir() { "dir" } else { "file" };
                    (t, m.len())
                }
                Err(_) => ("unknown", 0),
            };
            entries.push(format!("{:<6} {:>10}  {}", ftype, size, name));
        }

        entries.sort();
        if entries.is_empty() {
            Ok("(empty directory)".to_string())
        } else {
            Ok(entries.join("\n"))
        }
    }
}

// ─── Web Search Tool (via DuckDuckGo HTML) ──────────────────────────────────

/// Simple web search using DuckDuckGo HTML API.
pub struct WebSearchTool {
    client: reqwest::Client,
}

impl WebSearchTool {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("Mozilla/5.0 (compatible; ClawDesk/1.0)")
            .build()
            .unwrap_or_default();
        Self { client }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_search".into(),
            description: "Search the web for information. Returns search results with titles, URLs, and snippets.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 5)"
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or("missing 'query' argument")?;

        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        debug!(query, max_results, "WebSearchTool searching");

        // Use DuckDuckGo HTML (no API key required)
        // Manual percent-encoding to avoid extra dependency
        let encoded_query: String = query
            .chars()
            .map(|c| match c {
                'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
                ' ' => "+".to_string(),
                _ => format!("%{:02X}", c as u32),
            })
            .collect();
        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            encoded_query
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("search request failed: {e}"))?;

        let html = response
            .text()
            .await
            .map_err(|e| format!("failed to read search results: {e}"))?;

        // Simple HTML extraction — extract result blocks
        let mut results = Vec::new();
        for segment in html.split("class=\"result__a\"").skip(1).take(max_results) {
            // Extract href
            let href = if let Some(h) = segment.split("href=\"").nth(1) {
                h.split('"').next().unwrap_or("")
            } else {
                ""
            };

            // Extract title text (between first > and first <)
            let title = segment
                .split('>')
                .nth(1)
                .and_then(|s| s.split('<').next())
                .unwrap_or("Untitled");

            // Extract snippet from result__snippet
            let snippet = if let Some(snip_part) = segment.split("result__snippet").nth(1) {
                snip_part
                    .split('>')
                    .nth(1)
                    .and_then(|s| s.split('<').next())
                    .unwrap_or("")
                    .trim()
            } else {
                ""
            };

            if !href.is_empty() {
                results.push(format!("- [{}]({})\n  {}", title.trim(), href, snippet));
            }
        }

        if results.is_empty() {
            Ok(format!("No search results found for: {query}"))
        } else {
            Ok(format!(
                "Search results for \"{}\":\n\n{}",
                query,
                results.join("\n\n")
            ))
        }
    }
}

// ─── Memory Search Tool ─────────────────────────────────────────────────────

/// Searches the memory store for relevant past information.
/// This tool requires a callback to the memory manager (injected at registration).
pub struct MemorySearchTool {
    /// Async callback that performs the actual memory recall.
    /// Returns (content, score) pairs.
    recall_fn: std::sync::Arc<dyn Fn(String, usize) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<(String, f32)>> + Send>> + Send + Sync>,
}

impl MemorySearchTool {
    /// Create with an async recall callback.
    ///
    /// The callback is natively async, eliminating the
    /// sync-async bridge (`block_in_place`) that risked deadlock
    /// when the tokio thread pool was saturated.
    pub fn with_async_recall(
        recall_fn: std::sync::Arc<dyn Fn(String, usize) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<(String, f32)>> + Send>> + Send + Sync>,
    ) -> Self {
        Self { recall_fn }
    }

    /// Create from a synchronous callback (legacy compatibility).
    ///
    /// Wraps the sync callback in a futures::ready() to satisfy the
    /// async interface. Use `with_async_recall()` for new code.
    pub fn new(
        sync_fn: std::sync::Arc<dyn Fn(String, usize) -> Vec<(String, f32)> + Send + Sync>,
    ) -> Self {
        Self {
            recall_fn: std::sync::Arc::new(move |query: String, max: usize| {
                let result = sync_fn(query, max);
                Box::pin(async move { result })
            }),
        }
    }

    /// Create a no-op memory tool for when memory isn't available.
    pub fn noop() -> Self {
        Self {
            recall_fn: std::sync::Arc::new(|_, _| Box::pin(async { Vec::new() })),
        }
    }
}

#[async_trait]
impl Tool for MemorySearchTool {
    fn name(&self) -> &str {
        "memory_search"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "memory_search".into(),
            description: "Search through stored memories and past conversations for relevant information. Use when the user asks about something discussed previously.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query for memory recall"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of memory fragments to return (default: 5)"
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Memory]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or("missing 'query' argument")?;

        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        // Natively async — no block_in_place bridge needed.
        let results = (self.recall_fn)(query.to_string(), max_results).await;

        if results.is_empty() {
            Ok("No relevant memories found.".to_string())
        } else {
            let formatted: Vec<String> = results
                .iter()
                .enumerate()
                .map(|(i, (content, score))| {
                    format!("{}. [relevance: {:.2}] {}", i + 1, score, content)
                })
                .collect();
            Ok(format!(
                "Found {} relevant memories:\n\n{}",
                results.len(),
                formatted.join("\n\n")
            ))
        }
    }
}

// ─── Memory Store Tool ───────────────────────────────────────────────────────

/// Reactive memory tool — allows the LLM to explicitly save memories.
///
/// Complements `MemorySearchTool` (which reads). Together they form the
/// reactive memory loop: the LLM can search past context AND persist new
/// facts, decisions, user preferences, or task outcomes for future recall.
///
/// ## Architecture
///
/// ```text
/// LLM ─→ tool_call("memory_store", {content, tags})
///     ─→ MemoryStoreTool::execute()
///         ─→ store_fn(content, tags)
///             ─→ MemoryManager::remember(content, UserSaved, {tags})
///         ←─ Ok(memory_id)
/// ```
pub struct MemoryStoreTool {
    /// Async callback that performs the actual memory storage.
    /// Parameters: (content, tags) → Result<memory_id, error_message>.
    store_fn: std::sync::Arc<
        dyn Fn(
                String,
                Vec<String>,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
}

impl MemoryStoreTool {
    /// Create from an async callback to `MemoryManager::remember()`.
    pub fn with_async_store(
        store_fn: std::sync::Arc<
            dyn Fn(
                    String,
                    Vec<String>,
                )
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                + Send
                + Sync,
        >,
    ) -> Self {
        Self { store_fn }
    }

    /// Create a no-op memory store tool for when memory isn't available.
    pub fn noop() -> Self {
        Self {
            store_fn: std::sync::Arc::new(|_, _| {
                Box::pin(async { Ok("memory_store_noop".to_string()) })
            }),
        }
    }
}

#[async_trait]
impl Tool for MemoryStoreTool {
    fn name(&self) -> &str {
        "memory_store"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "memory_store".into(),
            description: "Save important information to long-term memory for future recall. \
                Use this to persist facts, decisions, user preferences, project context, \
                or task outcomes that should be remembered across conversations."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "The information to store in memory. Be specific and self-contained — include enough context so the memory is useful when recalled later."
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional tags for categorization (e.g. [\"preference\", \"project:acme\", \"decision\"])"
                    }
                },
                "required": ["content"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Memory]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("missing 'content' argument")?;

        if content.trim().is_empty() {
            return Err("content must not be empty".into());
        }

        let tags: Vec<String> = args
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let memory_id = (self.store_fn)(content.to_string(), tags.clone()).await?;

        Ok(format!(
            "Memory stored successfully (id: {}). Tags: [{}]",
            memory_id,
            if tags.is_empty() {
                "none".to_string()
            } else {
                tags.join(", ")
            }
        ))
    }
}

// ─── Messaging Tool ──────────────────────────────────────────────────────────

/// GAP-11: Built-in messaging tool — sends messages through channels.
///
/// Follows the OpenClaw `message` / `sessions_send` pattern:
/// - The LLM calls this tool when it wants to proactively send a message
///   to a specific channel or recipient.
/// - Execution delegates to an injected async callback (the gateway layer
///   wires the actual channel send).
/// - Sent messages are tracked for duplicate suppression (the runner checks
///   `AgentResponse.messaging_tool_sent` to avoid echoing tool-sent content).
///
/// ## Architecture
///
/// ```text
/// LLM ─→ tool_call("message_send", {to, channel, content})
///     ─→ MessageSendTool::execute()
///         ─→ send_fn(target, channel, content, media_urls)
///             ─→ channel gateway delivers to Telegram/Discord/Slack/etc.
/// ```
pub struct MessageSendTool {
    /// Async callback that performs the actual message delivery through the
    /// channel gateway. Parameters: (target, channel_id, content, media_urls).
    /// Returns Ok(delivery_id) or Err(error_message).
    send_fn: std::sync::Arc<
        dyn Fn(
                String,
                Option<String>,
                String,
                Vec<String>,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
}

/// Record of a message sent via the messaging tool, for duplicate suppression.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MessagingToolSend {
    /// Normalized target identifier (channel ID, user, thread).
    pub target: String,
    /// Channel provider used for delivery.
    pub channel: Option<String>,
    /// The message text that was sent.
    pub content: String,
    /// Media URLs attached to the message.
    pub media_urls: Vec<String>,
    /// Delivery ID returned by the gateway (for tracking/correlation).
    pub delivery_id: Option<String>,
}

/// Tracker for messaging tool sends — enables duplicate suppression.
///
/// Accumulated during `execute_loop()` and attached to `AgentResponse`.
/// The reply formatter uses this to strip payloads that duplicate
/// tool-sent content (following OpenClaw's `filterMessagingToolDuplicates`).
#[derive(Debug, Clone, Default)]
pub struct MessagingToolTracker {
    /// All successful sends during this agent run.
    sends: Vec<MessagingToolSend>,
    /// Normalized sent texts for O(1) substring-match lookups.
    normalized_texts: Vec<String>,
    /// Cap on tracked sends (FIFO eviction beyond this).
    max_tracked: usize,
}

impl MessagingToolTracker {
    pub fn new() -> Self {
        Self {
            sends: Vec::new(),
            normalized_texts: Vec::new(),
            max_tracked: 200,
        }
    }

    /// Record a successful send.
    pub fn record(&mut self, send: MessagingToolSend) {
        let normalized = Self::normalize_text(&send.content);
        self.sends.push(send);
        self.normalized_texts.push(normalized);

        // FIFO eviction when exceeding cap
        if self.sends.len() > self.max_tracked {
            self.sends.remove(0);
            self.normalized_texts.remove(0);
        }
    }

    /// Check if a reply text is a duplicate of a tool-sent message.
    ///
    /// Uses bidirectional substring containment (matching OpenClaw's
    /// `isMessagingToolDuplicate`). Minimum length threshold of 10 chars.
    pub fn is_duplicate(&self, reply_text: &str) -> bool {
        let normalized = Self::normalize_text(reply_text);
        if normalized.len() < 10 {
            return false;
        }
        for sent in &self.normalized_texts {
            if sent.len() < 10 {
                continue;
            }
            if normalized.contains(sent.as_str()) || sent.contains(normalized.as_str()) {
                return true;
            }
        }
        false
    }

    /// Check if any send targeted the same channel+target as the originator.
    pub fn sent_to_originator(&self, channel: Option<&str>, target: &str) -> bool {
        self.sends.iter().any(|s| {
            s.target == target
                && match (channel, &s.channel) {
                    (Some(ch), Some(sch)) => ch == sch,
                    (None, None) => true,
                    _ => false,
                }
        })
    }

    /// All recorded sends.
    pub fn sends(&self) -> &[MessagingToolSend] {
        &self.sends
    }

    /// Normalize text for comparison: lowercase, collapse whitespace, trim.
    fn normalize_text(text: &str) -> String {
        text.trim()
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl MessageSendTool {
    /// Create with an async send callback.
    ///
    /// The callback receives `(target, channel_id, content, media_urls)` and
    /// returns `Ok(delivery_id)` or `Err(error_msg)`.
    pub fn new(
        send_fn: std::sync::Arc<
            dyn Fn(
                    String,
                    Option<String>,
                    String,
                    Vec<String>,
                )
                    -> std::pin::Pin<
                        Box<dyn std::future::Future<Output = Result<String, String>> + Send>,
                    >
                + Send
                + Sync,
        >,
    ) -> Self {
        Self { send_fn }
    }

    /// Create a no-op messaging tool (for testing or when messaging is disabled).
    pub fn noop() -> Self {
        Self {
            send_fn: std::sync::Arc::new(|target, _channel, _content, _media| {
                Box::pin(async move {
                    Ok(format!("noop-delivery-{}", target))
                })
            }),
        }
    }
}

#[async_trait]
impl Tool for MessageSendTool {
    fn name(&self) -> &str {
        "message_send"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "message_send".into(),
            description: "Send a message to a specific channel or recipient. Use when you need to proactively deliver information to a user or channel (e.g., notifications, replies to external threads, cross-channel messages).".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "Target identifier — a channel ID, user ID, or thread ID to send the message to"
                    },
                    "channel": {
                        "type": "string",
                        "description": "Channel provider to send through (e.g., 'telegram', 'discord', 'slack'). If omitted, uses the originating channel."
                    },
                    "content": {
                        "type": "string",
                        "description": "The message content to send"
                    },
                    "media_urls": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of media URLs (images, files) to attach to the message"
                    }
                },
                "required": ["to", "content"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Messaging]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let target = args
            .get("to")
            .and_then(|v| v.as_str())
            .ok_or("missing 'to' argument")?
            .to_string();

        let channel = args
            .get("channel")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("missing 'content' argument")?
            .to_string();

        let media_urls: Vec<String> = args
            .get("media_urls")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        debug!(
            target = %target,
            channel = ?channel,
            content_len = content.len(),
            media_count = media_urls.len(),
            "MessageSendTool executing"
        );

        let delivery_id = (self.send_fn)(
            target.clone(),
            channel.clone(),
            content.clone(),
            media_urls.clone(),
        )
        .await?;

        // Return a structured result the runner can parse for tracking
        let result = json!({
            "status": "sent",
            "delivery_id": delivery_id,
            "target": target,
            "channel": channel,
            "content_length": content.len(),
            "media_count": media_urls.len(),
        });

        Ok(result.to_string())
    }
}

// ─── Registry Builder ────────────────────────────────────────────────────────

/// Register all built-in tools into a `ToolRegistry`.
///
/// Workspace scoping: if `workspace` is `Some`, file and shell tools are
/// confined to that directory. Otherwise they operate on the full filesystem.
pub fn register_builtin_tools(
    registry: &mut crate::tools::ToolRegistry,
    workspace: Option<PathBuf>,
) {
    use std::sync::Arc;

    registry.register(Arc::new(ShellTool::new(workspace.clone())));
    registry.register(Arc::new(HttpTool::new()));
    registry.register(Arc::new(FileReadTool::new(workspace.clone())));
    registry.register(Arc::new(FileWriteTool::new(workspace.clone())));
    registry.register(Arc::new(FileListTool::new(workspace)));
    registry.register(Arc::new(WebSearchTool::new()));
    // MemorySearchTool is registered separately via register_memory_tool()
    // because it needs a callback to the MemoryManager.
    // MessageSendTool is registered separately via register_messaging_tool()
    // because it needs a callback to the channel gateway.
}

// ─── GAP-7: Sub-Agent Tool ──────────────────────────────────────────────────

/// Tool that allows an agent to spawn a sub-agent for delegation.
///
/// The parent agent can delegate a task to another agent, wait for the result,
/// and incorporate it into its own response. Uses the `SpawnConfig` types from
/// `crate::subagent`.
pub struct SpawnSubAgentTool {
    /// Callback that actually spawns and runs the sub-agent.
    /// Receives (agent_id, task, timeout_secs) → Result<response_text, error>.
    spawn_fn: Arc<
        dyn Fn(
                String,
                String,
                u64,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
}

impl SpawnSubAgentTool {
    pub fn new(
        spawn_fn: Arc<
            dyn Fn(
                    String,
                    String,
                    u64,
                )
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                + Send
                + Sync,
        >,
    ) -> Self {
        Self { spawn_fn }
    }
}

#[async_trait::async_trait]
impl crate::tools::Tool for SpawnSubAgentTool {
    fn name(&self) -> &str {
        "spawn_subagent"
    }

    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "spawn_subagent".to_string(),
            description: "Delegate a task to another agent. The sub-agent runs to completion \
                           and returns its result text. Use this when the task requires a \
                           different agent's specialization."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "The ID of the agent to spawn for this task."
                    },
                    "task": {
                        "type": "string",
                        "description": "The task description / prompt to send to the sub-agent."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Maximum seconds to wait for the sub-agent (default: 120).",
                        "default": 120
                    }
                },
                "required": ["agent_id", "task"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let agent_id = args
            .get("agent_id")
            .and_then(|v| v.as_str())
            .ok_or("Missing required parameter: agent_id")?
            .to_string();

        let task = args
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or("Missing required parameter: task")?
            .to_string();

        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(120);

        (self.spawn_fn)(agent_id, task, timeout_secs).await
    }
}

/// Register the sub-agent spawn tool with an async spawn callback (GAP-7).
///
/// The callback receives `(agent_id, task, timeout_secs)` and returns
/// `Ok(response_text)` from the sub-agent or `Err(error_message)`.
pub fn register_subagent_tool(
    registry: &mut crate::tools::ToolRegistry,
    spawn_fn: Arc<
        dyn Fn(
                String,
                String,
                u64,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
) {
    registry.register(Arc::new(SpawnSubAgentTool::new(spawn_fn)));
}

/// Register the messaging tool with an async send callback (GAP-11).
///
/// The callback receives `(target, channel_id, content, media_urls)` and
/// returns `Ok(delivery_id)` or `Err(error_message)`.
pub fn register_messaging_tool(
    registry: &mut crate::tools::ToolRegistry,
    send_fn: std::sync::Arc<
        dyn Fn(
                String,
                Option<String>,
                String,
                Vec<String>,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
) {
    use std::sync::Arc;
    registry.register(Arc::new(MessageSendTool::new(send_fn)));
}

/// Register the memory search tool with an async recall callback.
pub fn register_memory_tool_async(
    registry: &mut crate::tools::ToolRegistry,
    recall_fn: std::sync::Arc<dyn Fn(String, usize) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<(String, f32)>> + Send>> + Send + Sync>,
) {
    use std::sync::Arc;
    registry.register(Arc::new(MemorySearchTool::with_async_recall(recall_fn)));
}

/// Register the memory search tool with a sync recall callback (legacy).
pub fn register_memory_tool(
    registry: &mut crate::tools::ToolRegistry,
    recall_fn: std::sync::Arc<dyn Fn(String, usize) -> Vec<(String, f32)> + Send + Sync>,
) {
    use std::sync::Arc;
    registry.register(Arc::new(MemorySearchTool::new(recall_fn)));
}

/// Register the memory store tool with an async store callback.
///
/// The callback receives `(content, tags)` and should persist via
/// `MemoryManager::remember()`.
pub fn register_memory_store_tool_async(
    registry: &mut crate::tools::ToolRegistry,
    store_fn: std::sync::Arc<
        dyn Fn(
                String,
                Vec<String>,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
) {
    use std::sync::Arc;
    registry.register(Arc::new(MemoryStoreTool::with_async_store(store_fn)));
}

// ─── A2A Sessions Send Tool ─────────────────────────────────────────────────

/// Tool that sends a message/task to another agent session via A2A protocol.
///
/// Unlike `spawn_subagent` (which spawns in-process), this tool dispatches
/// over the A2A HTTP bridge to any registered agent — local or remote.
///
/// ## Flow
///
/// ```text
/// LLM ─→ tool_call("sessions_send", {target_agent, message, skill_id})
///     ─→ SessionsSendTool::execute()
///         ─→ send_fn(target_agent, message, skill_id)
///             ─→ A2AHandler::send_task() → HTTP POST /a2a/tasks/send
///             ─→ poll task until completion
///         ←─ task result text
/// ```
pub struct SessionsSendTool {
    /// Async callback: (target_agent, message, skill_id) → Result<response>
    send_fn: Arc<
        dyn Fn(
                String,
                String,
                Option<String>,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
}

impl SessionsSendTool {
    pub fn new(
        send_fn: Arc<
            dyn Fn(
                    String,
                    String,
                    Option<String>,
                )
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                + Send
                + Sync,
        >,
    ) -> Self {
        Self { send_fn }
    }
}

#[async_trait]
impl Tool for SessionsSendTool {
    fn name(&self) -> &str {
        "sessions_send"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "sessions_send".into(),
            description: "Send a task or message to another agent via A2A protocol. \
                Use when you need to delegate work to a specialized agent \
                (e.g., code review, web search, data analysis). The target \
                agent processes the request and returns a result."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "target_agent": {
                        "type": "string",
                        "description": "The ID of the target agent to send the task to"
                    },
                    "message": {
                        "type": "string",
                        "description": "The task description or message to send to the target agent"
                    },
                    "skill_id": {
                        "type": "string",
                        "description": "Optional specific skill to invoke on the target agent"
                    }
                },
                "required": ["target_agent", "message"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ExternalApi]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let target_agent = args
            .get("target_agent")
            .and_then(|v| v.as_str())
            .ok_or("Missing required parameter: target_agent")?
            .to_string();

        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or("Missing required parameter: message")?
            .to_string();

        let skill_id = args
            .get("skill_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        debug!(
            target = %target_agent,
            message_len = message.len(),
            skill = ?skill_id,
            "SessionsSendTool executing"
        );

        (self.send_fn)(target_agent, message, skill_id).await
    }
}

/// Register the A2A sessions_send tool with an async callback.
///
/// The callback receives `(target_agent, message, skill_id)` and returns
/// `Ok(response_text)` from the target agent or `Err(error_message)`.
pub fn register_sessions_send_tool(
    registry: &mut crate::tools::ToolRegistry,
    send_fn: Arc<
        dyn Fn(
                String,
                String,
                Option<String>,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
) {
    registry.register(Arc::new(SessionsSendTool::new(send_fn)));
}

// ─── A2A Agent List Tool (Session Visibility) ───────────────────────────────

/// Tool that lists available agents and their capabilities from the A2A directory.
///
/// This gives the agent visibility into what other agents are available for
/// delegation, enabling intelligent routing decisions.
pub struct AgentsListTool {
    /// Async callback that returns a JSON string listing available agents.
    list_fn: Arc<
        dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
}

impl AgentsListTool {
    pub fn new(
        list_fn: Arc<
            dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                + Send
                + Sync,
        >,
    ) -> Self {
        Self { list_fn }
    }
}

#[async_trait]
impl Tool for AgentsListTool {
    fn name(&self) -> &str {
        "agents_list"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "agents_list".into(),
            description: "List all available agents in the A2A directory with their capabilities and status. \
                Use this to discover which agents are available for task delegation before using sessions_send."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![] // Read-only, no special capabilities needed
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        debug!("AgentsListTool executing");
        (self.list_fn)().await
    }
}

/// Register the agents_list tool with an async callback (Session Visibility).
///
/// The callback returns a JSON string with the list of agents.
pub fn register_agents_list_tool(
    registry: &mut crate::tools::ToolRegistry,
    list_fn: Arc<
        dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
) {
    registry.register(Arc::new(AgentsListTool::new(list_fn)));
}

// ─── GAP-1: Dynamic Agent Spawning ─────────────────────────────────────────

/// Tool capability delegation mode for ephemeral agents.
///
/// Models a bounded lattice: `None ⊆ Only(S) ⊆ Inherit` under subset ordering.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", content = "names", rename_all = "snake_case")]
pub enum ToolAccess {
    /// Inherit all tools from the parent agent (default).
    Inherit,
    /// No tools — the child operates in pure text/reasoning mode.
    None,
    /// Only the named tools (allowlist). Names not in the parent's registry are silently ignored.
    Only(Vec<String>),
}

impl Default for ToolAccess {
    fn default() -> Self {
        Self::Inherit
    }
}

/// Maximum timeout for a dynamic spawn (seconds).
const MAX_DYNAMIC_TIMEOUT_SECS: u64 = 600;
/// Maximum tool rounds for a dynamically spawned agent.
const MAX_DYNAMIC_TOOL_ROUNDS: usize = 20;
/// Default timeout for dynamic spawn (seconds).
const DEFAULT_DYNAMIC_TIMEOUT_SECS: u64 = 120;
/// Default tool rounds for dynamic spawn.
const DEFAULT_DYNAMIC_TOOL_ROUNDS: usize = 5;

/// LLM-parameterized request for ephemeral agent creation.
///
/// All fields except `task` are optional — the LLM controls cost, capability,
/// and behavioral constraints through the optional overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicSpawnRequest {
    /// The task description / prompt — serves as both the child's user message
    /// and the semantic core of its system prompt.
    pub task: String,
    /// Optional human-readable label for this spawn (used in logs and traces).
    #[serde(default)]
    pub label: Option<String>,
    /// Model override. When `None`, inherits the parent's model.
    #[serde(default)]
    pub model: Option<String>,
    /// Tool capability restriction (default: Inherit).
    #[serde(default)]
    pub tools: ToolAccess,
    /// Maximum seconds to wait (clamped to MAX_DYNAMIC_TIMEOUT_SECS).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Maximum tool call rounds (clamped to MAX_DYNAMIC_TOOL_ROUNDS).
    #[serde(default)]
    pub max_tool_rounds: Option<usize>,
}

impl DynamicSpawnRequest {
    /// Effective timeout, clamped to the hard cap.
    pub fn effective_timeout(&self) -> u64 {
        self.timeout_secs
            .unwrap_or(DEFAULT_DYNAMIC_TIMEOUT_SECS)
            .min(MAX_DYNAMIC_TIMEOUT_SECS)
    }

    /// Effective tool round budget, clamped to the hard cap.
    pub fn effective_tool_rounds(&self) -> usize {
        self.max_tool_rounds
            .unwrap_or(DEFAULT_DYNAMIC_TOOL_ROUNDS)
            .min(MAX_DYNAMIC_TOOL_ROUNDS)
    }
}

/// Tool that allows an agent to create ephemeral sub-agents on the fly.
///
/// Unlike `SpawnSubAgentTool` (which requires a pre-registered `agent_id`),
/// this tool lets the LLM parameterize agent creation entirely at runtime:
/// task description, model choice, tool access, and round budget.
pub struct DynamicSpawnTool {
    /// Callback that creates and runs an ephemeral sub-agent.
    /// Receives a `DynamicSpawnRequest` → Result<response_text, error>.
    spawn_fn: Arc<
        dyn Fn(
                DynamicSpawnRequest,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
}

impl DynamicSpawnTool {
    pub fn new(
        spawn_fn: Arc<
            dyn Fn(
                    DynamicSpawnRequest,
                )
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                + Send
                + Sync,
        >,
    ) -> Self {
        Self { spawn_fn }
    }
}

#[async_trait]
impl Tool for DynamicSpawnTool {
    fn name(&self) -> &str {
        "dynamic_spawn"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "dynamic_spawn".into(),
            description: "Create an ephemeral specialist sub-agent for a specific task. \
                Unlike spawn_subagent (which delegates to a pre-configured agent), this \
                creates a fresh agent whose role is defined entirely by the task description. \
                Use this when you need an ad-hoc specialist — e.g., a focused researcher, \
                data analyst, or code reviewer — that doesn't exist as a pre-defined agent. \
                The sub-agent runs to completion and its result is returned to you."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "The task description for the sub-agent. This defines \
                            both what the agent should do AND its role/specialization."
                    },
                    "label": {
                        "type": "string",
                        "description": "Short label for this agent (e.g. 'code-reviewer', \
                            'summarizer'). Used in logs, not sent to the model."
                    },
                    "model": {
                        "type": "string",
                        "description": "Model override (e.g. 'claude-haiku' for cheaper tasks). \
                            If omitted, inherits the parent's model."
                    },
                    "tools": {
                        "type": "object",
                        "description": "Tool access policy. Omit for full inheritance. \
                            Use {\"mode\": \"none\"} for text-only, or \
                            {\"mode\": \"only\", \"names\": [\"tool1\", \"tool2\"]} for allowlist.",
                        "properties": {
                            "mode": {
                                "type": "string",
                                "enum": ["inherit", "none", "only"]
                            },
                            "names": {
                                "type": "array",
                                "items": { "type": "string" }
                            }
                        }
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Max seconds to wait (default: 120, max: 600).",
                        "default": 120
                    },
                    "max_tool_rounds": {
                        "type": "integer",
                        "description": "Max tool call rounds (default: 5, max: 20).",
                        "default": 5
                    }
                },
                "required": ["task"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let request: DynamicSpawnRequest = serde_json::from_value(args)
            .map_err(|e| format!("Invalid dynamic_spawn parameters: {e}"))?;

        if request.task.trim().is_empty() {
            return Err("'task' must not be empty".into());
        }

        debug!(
            task_len = request.task.len(),
            label = ?request.label,
            model = ?request.model,
            timeout = request.effective_timeout(),
            tool_rounds = request.effective_tool_rounds(),
            "DynamicSpawnTool executing"
        );

        (self.spawn_fn)(request).await
    }
}

/// Register the dynamic agent spawn tool (GAP-1).
///
/// The callback receives a `DynamicSpawnRequest` and returns
/// `Ok(response_text)` from the ephemeral agent or `Err(error_message)`.
pub fn register_dynamic_spawn_tool(
    registry: &mut crate::tools::ToolRegistry,
    spawn_fn: Arc<
        dyn Fn(
                DynamicSpawnRequest,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
) {
    registry.register(Arc::new(DynamicSpawnTool::new(spawn_fn)));
}
