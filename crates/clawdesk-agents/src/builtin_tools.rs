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
/// Aligns with the `exec` tool: skills reference `shell_exec` in their
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

        // Exec policy enforcement — validate command before execution.
        // Skip policy checks for internal control commands (bg_status, bg_kill).
        if !command.starts_with("bg_status ") && !command.starts_with("bg_kill ") {
            use crate::exec_policy::{ExecPolicy, ExecPolicyConfig, ExecVerdict};
            let exec_policy = ExecPolicy::new(ExecPolicyConfig::default());
            if let ExecVerdict::Deny { reason } = exec_policy.check(command) {
                warn!(command, reason = %reason, "ShellTool: command blocked by exec policy");
                return Err(format!("command blocked by exec policy: {}", reason));
            }

            // Taint tracking — check for embedded secrets/credentials.
            use clawdesk_types::taint::{TaintSink, TaintedValue, TaintLabel};
            let tainted = TaintedValue::new(command.to_string(), TaintLabel::ToolOutput);
            if let Err(violation) = TaintSink::shell_exec_sink().validate(&tainted) {
                warn!(command, violation = %violation, "ShellTool: command blocked by taint policy");
                return Err(format!("command blocked: {}", violation));
            }
        }

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(command);
        if let Some(ref dir) = working_dir {
            cmd.current_dir(dir);
        }
        // Prepend sidecar/bundled tool directories to PATH so agents can
        // find co-distributed binaries like `gws` without explicit paths.
        {
            let mut extra_paths = Vec::new();
            // Tauri sidecar directory (next to executable)
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent() {
                    extra_paths.push(dir.to_path_buf());
                }
            }
            // Workspace tools/bundled/ (dev mode)
            let bundled = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tools/bundled");
            if bundled.exists() {
                extra_paths.push(bundled);
            }
            if !extra_paths.is_empty() {
                let current_path = std::env::var("PATH").unwrap_or_default();
                let prepend: Vec<String> = extra_paths.iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect();
                let new_path = format!("{}:{}", prepend.join(":"), current_path);
                cmd.env("PATH", new_path);
            }
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
                let (code, stdout, stderr) = tokio::join!(
                    entry.exit_code.lock(),
                    entry.stdout_buf.lock(),
                    entry.stderr_buf.lock()
                );
                let elapsed = entry.started_at.elapsed().as_secs();
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
                            // Safe wrapper: libc::kill with error check.  The pid is valid
                            // because child.id() returns Some only while the child is alive.
                            let ret = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                            if ret != 0 {
                                tracing::warn!(pid, "SIGTERM delivery failed (errno={})", std::io::Error::last_os_error());
                            }
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

        // Taint tracking — check URL for embedded secrets/credentials.
        {
            use clawdesk_types::taint::{TaintSink, TaintedValue, TaintLabel};
            let tainted_url = TaintedValue::new(url.to_string(), TaintLabel::ToolOutput);
            if let Err(violation) = TaintSink::api_key_sink().validate(&tainted_url) {
                warn!(url, violation = %violation, "HttpTool: URL blocked by taint policy");
                return Err(format!("URL blocked: {}", violation));
            }
        }

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
            // Find a valid char boundary at or before max_response_bytes
            let mut end = self.max_response_bytes;
            while end > 0 && !body.is_char_boundary(end) {
                end -= 1;
            }
            format!(
                "{}...\n[truncated: {} bytes total]",
                &body[..end],
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
            let mut end = self.max_bytes;
            while end > 0 && !content.is_char_boundary(end) {
                end -= 1;
            }
            format!(
                "{}...\n[truncated: {} bytes total, {} lines]",
                &content[..end],
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

// ─── File Edit Tool (search/replace) ────────────────────────────────────────

/// Precise search/replace editing tool, inspired by pi-mono's edit protocol.
/// Finds exact text in a file and replaces it. Much safer than full file_write
/// for modifying existing files — prevents accidental overwrites and makes
/// changes reviewable.
pub struct FileEditTool {
    workspace: Option<PathBuf>,
}

impl FileEditTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self { workspace }
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        if let Some(ref ws) = self.workspace {
            let target = Path::new(path);
            if target.is_absolute() {
                target.to_path_buf()
            } else {
                ws.join(target)
            }
        } else {
            PathBuf::from(path)
        }
    }
}

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "file_edit"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "file_edit".into(),
            description: "Edit a file by replacing exact text. Use this instead of file_write when modifying existing files — it's safer and more precise. The old_text must match exactly (including whitespace and indentation). For creating new files, use file_write instead.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit (relative to workspace)"
                    },
                    "old_text": {
                        "type": "string",
                        "description": "Exact text to find and replace. Must match the file content precisely, including whitespace and indentation. Include 2-3 lines of context before and after the target to ensure unique matching."
                    },
                    "new_text": {
                        "type": "string",
                        "description": "Text to replace old_text with. Can be empty to delete the matched text."
                    }
                },
                "required": ["path", "old_text", "new_text"]
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
        let old_text = args
            .get("old_text")
            .and_then(|v| v.as_str())
            .ok_or("missing 'old_text' argument")?;
        let new_text = args
            .get("new_text")
            .and_then(|v| v.as_str())
            .ok_or("missing 'new_text' argument")?;

        let resolved = self.resolve_path(path_str);

        // Read the file as bytes to handle BOM
        let raw_bytes = tokio::fs::read(&resolved)
            .await
            .map_err(|e| format!("failed to read file '{}': {}", path_str, e))?;

        // Strip UTF-8 BOM if present (LLMs won't include it in old_text)
        let content_str = if raw_bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            String::from_utf8_lossy(&raw_bytes[3..]).into_owned()
        } else {
            String::from_utf8_lossy(&raw_bytes).into_owned()
        };

        // Detect line endings for preservation
        let uses_crlf = content_str.contains("\r\n");

        // Normalize to LF for matching
        let normalized = content_str.replace("\r\n", "\n");
        let old_normalized = old_text.replace("\r\n", "\n");
        let new_normalized = new_text.replace("\r\n", "\n");

        // Try exact match first
        let match_count = normalized.matches(&old_normalized).count();

        if match_count == 1 {
            // Exact match — perform replacement
            let new_content = normalized.replacen(&old_normalized, &new_normalized, 1);

            // Generate unified diff before writing
            let diff_output = generate_unified_diff(&normalized, &new_content, path_str);

            // Restore original line endings if file used CRLF
            let final_content = if uses_crlf {
                new_content.replace('\n', "\r\n")
            } else {
                new_content
            };

            // Restore BOM if original had one
            let write_bytes = if raw_bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
                let mut bom = vec![0xEF, 0xBB, 0xBF];
                bom.extend(final_content.as_bytes());
                bom
            } else {
                final_content.into_bytes()
            };

            tokio::fs::write(&resolved, &write_bytes)
                .await
                .map_err(|e| format!("failed to write file: {e}"))?;

            let old_lines = old_normalized.lines().count();
            let new_lines = new_normalized.lines().count();
            let diff_info = if new_text.is_empty() {
                format!("Deleted {} lines", old_lines)
            } else if old_text.is_empty() {
                format!("Inserted {} lines", new_lines)
            } else {
                format!("Replaced {} lines with {} lines", old_lines, new_lines)
            };

            return Ok(format!(
                "Successfully edited '{}': {}\n\n{}",
                path_str, diff_info, diff_output
            ));
        }

        if match_count > 1 {
            return Err(format!(
                "old_text matches {} locations in '{}'. Include more surrounding context \
                (2-3 lines before and after) to make the match unique.",
                match_count, path_str
            ));
        }

        // match_count == 0 — try fuzzy matching
        // Normalize Unicode: smart quotes → ASCII, dashes → hyphen, special spaces → space
        let fuzzy_content = normalize_for_fuzzy_match(&normalized);
        let fuzzy_old = normalize_for_fuzzy_match(&old_normalized);

        let fuzzy_count = fuzzy_content.matches(&fuzzy_old).count();

        if fuzzy_count == 1 {
            // Fuzzy match found — perform replacement in fuzzy-normalized space
            let fuzzy_new = normalize_for_fuzzy_match(&new_normalized);
            let new_content = fuzzy_content.replacen(&fuzzy_old, &fuzzy_new, 1);

            let diff_output = generate_unified_diff(&fuzzy_content, &new_content, path_str);

            let final_content = if uses_crlf {
                new_content.replace('\n', "\r\n")
            } else {
                new_content
            };

            let write_bytes = if raw_bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
                let mut bom = vec![0xEF, 0xBB, 0xBF];
                bom.extend(final_content.as_bytes());
                bom
            } else {
                final_content.into_bytes()
            };

            tokio::fs::write(&resolved, &write_bytes)
                .await
                .map_err(|e| format!("failed to write file: {e}"))?;

            let old_lines = old_normalized.lines().count();
            let new_lines = new_normalized.lines().count();

            return Ok(format!(
                "Successfully edited '{}' (fuzzy match — whitespace/Unicode normalized): Replaced {} lines with {} lines\n\n{}",
                path_str, old_lines, new_lines, diff_output
            ));
        }

        if fuzzy_count > 1 {
            return Err(format!(
                "old_text fuzzy-matches {} locations in '{}'. Include more surrounding context \
                (2-3 lines before and after) to make the match unique.",
                fuzzy_count, path_str
            ));
        }

        // Try whitespace-trimmed line matching for a helpful error
        let old_lines: Vec<&str> = old_normalized.lines().map(|l| l.trim()).collect();
        let content_lines: Vec<&str> = normalized.lines().collect();

        let mut found = false;
        for window_start in 0..content_lines.len().saturating_sub(old_lines.len().saturating_sub(1)) {
            let window = &content_lines[window_start..std::cmp::min(window_start + old_lines.len(), content_lines.len())];
            let trimmed_window: Vec<&str> = window.iter().map(|l| l.trim()).collect();
            if trimmed_window == old_lines {
                found = true;
                break;
            }
        }

        if found {
            return Err(format!(
                "old_text not found as exact match in '{}'. A similar block exists but whitespace/indentation differs. \
                Make sure old_text matches the file content exactly, including leading spaces and tabs.",
                path_str
            ));
        }

        Err(format!(
            "old_text not found in '{}'. The text you're trying to replace does not exist in the file. \
            Use file_read to check the current file content first.",
            path_str
        ))
    }
}

/// Normalize text for fuzzy matching — strip trailing whitespace, normalize
/// Unicode quotes/dashes/spaces to their ASCII equivalents.
fn normalize_for_fuzzy_match(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for line in text.split('\n') {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(line.trim_end());
    }
    result
        // Smart single quotes → '
        .replace('\u{2018}', "'")
        .replace('\u{2019}', "'")
        .replace('\u{201A}', "'")
        .replace('\u{201B}', "'")
        // Smart double quotes → "
        .replace('\u{201C}', "\"")
        .replace('\u{201D}', "\"")
        .replace('\u{201E}', "\"")
        .replace('\u{201F}', "\"")
        // Various dashes/hyphens → -
        .replace('\u{2010}', "-")
        .replace('\u{2011}', "-")
        .replace('\u{2012}', "-")
        .replace('\u{2013}', "-")
        .replace('\u{2014}', "-")
        .replace('\u{2015}', "-")
        .replace('\u{2212}', "-")
        // Special spaces → regular space
        .replace('\u{00A0}', " ")
        .replace('\u{202F}', " ")
        .replace('\u{205F}', " ")
        .replace('\u{3000}', " ")
}

/// Generate a minimal unified diff between old and new content.
fn generate_unified_diff(old_content: &str, new_content: &str, filename: &str) -> String {
    let old_lines: Vec<&str> = old_content.lines().collect();
    let new_lines: Vec<&str> = new_content.lines().collect();

    let mut diff = String::new();
    diff.push_str(&format!("--- a/{}\n", filename));
    diff.push_str(&format!("+++ b/{}\n", filename));

    // Find first and last changed line
    let mut first_change = None;
    let mut last_change = 0;
    let max_len = old_lines.len().max(new_lines.len());

    for i in 0..max_len {
        let old_line = old_lines.get(i).copied();
        let new_line = new_lines.get(i).copied();
        if old_line != new_line {
            if first_change.is_none() {
                first_change = Some(i);
            }
            last_change = i;
        }
    }

    let Some(first) = first_change else {
        return String::from("(no changes)");
    };

    // Show 3 lines of context
    let ctx = 3;
    let start = first.saturating_sub(ctx);
    let end = (last_change + ctx + 1).min(max_len);

    diff.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        start + 1,
        (end).min(old_lines.len()).saturating_sub(start),
        start + 1,
        (end).min(new_lines.len()).saturating_sub(start),
    ));

    for i in start..end {
        let old_line = old_lines.get(i).copied();
        let new_line = new_lines.get(i).copied();
        match (old_line, new_line) {
            (Some(o), Some(n)) if o == n => {
                diff.push_str(&format!(" {}\n", o));
            }
            (Some(o), Some(n)) => {
                diff.push_str(&format!("-{}\n", o));
                diff.push_str(&format!("+{}\n", n));
            }
            (Some(o), None) => {
                diff.push_str(&format!("-{}\n", o));
            }
            (None, Some(n)) => {
                diff.push_str(&format!("+{}\n", n));
            }
            (None, None) => {}
        }
    }

    diff
}

// ─── Grep Search Tool ───────────────────────────────────────────────────────

/// Search for text patterns in files. Essential for understanding codebases
/// and finding relevant code before editing.
pub struct GrepTool {
    workspace: Option<PathBuf>,
}

impl GrepTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "grep".into(),
            description: "Search for a text pattern in files. Returns matching lines with file paths and line numbers. Use this to find code, understand codebase structure, and locate things to edit.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Text or regex pattern to search for"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search in (relative to workspace, default: workspace root)"
                    },
                    "include": {
                        "type": "string",
                        "description": "File extension filter, e.g. '*.rs' or '*.ts'"
                    }
                },
                "required": ["pattern"]
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
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or("missing 'pattern' argument")?;

        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        let include_filter = args
            .get("include")
            .and_then(|v| v.as_str());

        let target = if let Some(ref ws) = self.workspace {
            ws.join(path_str)
        } else {
            PathBuf::from(path_str)
        };

        // Build grep command
        let mut cmd = tokio::process::Command::new("grep");
        cmd.arg("-rn")         // recursive + line numbers
           .arg("--color=never")
           .arg("-I");          // skip binary files

        if let Some(inc) = include_filter {
            cmd.arg("--include").arg(inc);
        }

        // Limit output to prevent context overflow
        cmd.arg("-m").arg("50"); // max 50 matches per file

        cmd.arg("--").arg(pattern).arg(&target);

        let output = cmd
            .output()
            .await
            .map_err(|e| format!("grep failed: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if stdout.is_empty() {
            if !stderr.is_empty() {
                return Err(format!("grep error: {}", stderr.trim()));
            }
            return Ok(format!("No matches found for '{}' in {}", pattern, path_str));
        }

        // Strip workspace prefix from paths for readability
        let result = if let Some(ref ws) = self.workspace {
            let prefix = format!("{}/", ws.display());
            stdout
                .lines()
                .map(|line| line.strip_prefix(&prefix).unwrap_or(line))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            stdout.to_string()
        };

        // Truncate if very long
        if result.len() > 8000 {
            let truncated: String = result.chars().take(8000).collect();
            Ok(format!("{}\n\n... (output truncated, {} total chars)", truncated, result.len()))
        } else {
            Ok(result)
        }
    }
}

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

/// Built-in messaging tool — sends messages through channels.
///
/// Follows the `message` / `sessions_send` pattern:
/// - The LLM calls this tool when it wants to proactively send a message
///   to a specific channel or recipient.
/// - Execution delegates to an injected async callback (the gateway layer
///   wires the actual channel send).
/// - Sent messages are tracked for duplicate suppression (the runner checks
///   `AgentResponse.messaging_tool_sent` to avoid echoing tool-sent content).
// ─── Memory Forget Tool ──────────────────────────────────────────────────────

/// Allows the LLM to delete specific memories — stale, incorrect, or
/// user-requested deletions. Completes the memory lifecycle:
/// search → store → forget.
///
/// ## Architecture
///
/// ```text
/// LLM ─→ tool_call("memory_forget", {memory_id})
///     ─→ MemoryForgetTool::execute()
///         ─→ forget_fn(memory_id)
///             ─→ MemoryManager::forget(id)
///         ←─ Ok("deleted")
/// ```
pub struct MemoryForgetTool {
    /// Async callback that performs the actual memory deletion.
    forget_fn: std::sync::Arc<
        dyn Fn(
                String,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
            + Send
            + Sync,
    >,
}

impl MemoryForgetTool {
    /// Create from an async callback to `MemoryManager::forget()`.
    pub fn with_async_forget(
        forget_fn: std::sync::Arc<
            dyn Fn(
                    String,
                )
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
                + Send
                + Sync,
        >,
    ) -> Self {
        Self { forget_fn }
    }

    /// Create a no-op forget tool for when memory isn't available.
    pub fn noop() -> Self {
        Self {
            forget_fn: std::sync::Arc::new(|_| {
                Box::pin(async { Ok(()) })
            }),
        }
    }
}

#[async_trait]
impl Tool for MemoryForgetTool {
    fn name(&self) -> &str {
        "memory_forget"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "memory_forget".into(),
            description: "Delete a specific memory by its ID. Use when the user asks you to \
                forget something, when information is outdated, or when correcting stored facts. \
                Get the memory_id from memory_search results."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "memory_id": {
                        "type": "string",
                        "description": "The ID of the memory to delete (from memory_search results)"
                    }
                },
                "required": ["memory_id"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Memory]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let memory_id = args
            .get("memory_id")
            .and_then(|v| v.as_str())
            .ok_or("missing 'memory_id' argument")?;

        if memory_id.trim().is_empty() {
            return Err("memory_id must not be empty".into());
        }

        (self.forget_fn)(memory_id.to_string()).await?;

        Ok(format!("Memory {} deleted successfully.", memory_id))
    }
}

// ─── Messaging Tool ──────────────────────────────────────────────────────────

/// Built-in messaging tool — sends messages through channels.
///
/// Follows the `message` / `sessions_send` pattern:
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
    /// Typed async port for message delivery.
    send_fn: crate::port::AsyncPort<crate::port::MessageSendRequest, Result<String, String>>,
    /// Actually-connected channel names (populated from ChannelRegistry).
    /// Used in the tool schema so the LLM only sees channels that exist.
    available_channels: Vec<String>,
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
/// tool-sent content (following the `filterMessagingToolDuplicates`).
///
/// ## Media deduplication
/// Tracks sent media URLs via a `HashSet` for O(1) lookup. Prevents double
/// delivery when the same media is sent via a tool call and also appears
/// in the final response `media_urls`.
#[derive(Debug, Clone, Default)]
pub struct MessagingToolTracker {
    /// All successful sends during this agent run.
    sends: Vec<MessagingToolSend>,
    /// Normalized sent texts for O(1) substring-match lookups.
    normalized_texts: Vec<String>,
    /// Canonicalized media URLs that have been sent via tool calls.
    /// O(1) amortized insert/lookup via FxHashSet.
    sent_media_urls: rustc_hash::FxHashSet<String>,
    /// Cap on tracked sends (FIFO eviction beyond this).
    max_tracked: usize,
}

impl MessagingToolTracker {
    pub fn new() -> Self {
        Self {
            sends: Vec::new(),
            normalized_texts: Vec::new(),
            sent_media_urls: rustc_hash::FxHashSet::default(),
            max_tracked: 200,
        }
    }

    /// Record a successful send.
    pub fn record(&mut self, send: MessagingToolSend) {
        let normalized = Self::normalize_text(&send.content);

        // Track media URLs for deduplication
        for url in &send.media_urls {
            self.sent_media_urls.insert(Self::canonicalize_url(url));
        }

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
    /// Uses bidirectional substring containment (matching the    /// `isMessagingToolDuplicate`). Minimum length threshold of 10 chars.
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

    /// Check if a media URL was already sent via a tool call.
    ///
    /// O(1) amortized lookup. Uses canonicalized URL for comparison,
    /// handling trailing slashes, query parameter ordering differences, etc.
    pub fn is_media_duplicate(&self, url: &str) -> bool {
        self.sent_media_urls.contains(&Self::canonicalize_url(url))
    }

    /// Filter a list of media URLs, removing any already sent via tool calls.
    pub fn filter_duplicate_media(&self, urls: &[String]) -> Vec<String> {
        urls.iter()
            .filter(|url| !self.is_media_duplicate(url))
            .cloned()
            .collect()
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

    /// Canonicalize a URL for dedup comparison.
    ///
    /// Strips trailing slashes, normalizes to lowercase for scheme/host,
    /// and removes fragment identifiers.
    fn canonicalize_url(url: &str) -> String {
        let url = url.trim();
        // Remove fragment
        let url = url.split('#').next().unwrap_or(url);
        // Remove trailing slash (but not for root paths)
        let url = if url.len() > 1 { url.trim_end_matches('/') } else { url };
        url.to_string()
    }
}

impl MessageSendTool {
    /// Create with a typed async port for message delivery.
    ///
    /// `available_channels` is the list of actually-connected channel names
    /// (e.g., `["telegram", "discord"]`) from `ChannelRegistry`.
    pub fn new(
        send_fn: crate::port::AsyncPort<crate::port::MessageSendRequest, Result<String, String>>,
        available_channels: Vec<String>,
    ) -> Self {
        Self { send_fn, available_channels }
    }

    /// Create a no-op messaging tool (for testing or when messaging is disabled).
    pub fn noop() -> Self {
        Self {
            send_fn: std::sync::Arc::new(|req| {
                Box::pin(async move {
                    Ok(format!("noop-delivery-{}", req.target))
                })
            }),
            available_channels: vec![],
        }
    }
}

#[async_trait]
impl Tool for MessageSendTool {
    fn name(&self) -> &str {
        "message_send"
    }

    fn schema(&self) -> ToolSchema {
        // Build dynamic channel description from actually-connected channels
        let channel_desc = if self.available_channels.is_empty() {
            "Channel name (e.g., 'telegram', 'discord', 'slack')".to_string()
        } else {
            let names: Vec<String> = self.available_channels.iter()
                .map(|c| format!("'{}'", c))
                .collect();
            format!("Connected channel name: {}", names.join(", "))
        };

        ToolSchema {
            name: "message_send".into(),
            description: "Send a message to another channel. Just provide the channel name and content — the system automatically routes to the correct chat. Do NOT ask the user for channel IDs, chat IDs, or any numeric identifiers.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "channel": {
                        "type": "string",
                        "description": channel_desc
                    },
                    "content": {
                        "type": "string",
                        "description": "The message text to send"
                    },
                    "to": {
                        "type": "string",
                        "description": "Optional. Defaults to the last active chat on the channel. Only set if you have a specific numeric ID.",
                        "default": "default"
                    }
                },
                "required": ["channel", "content"]
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
            .unwrap_or("default")
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

        let delivery_id = (self.send_fn)(crate::port::MessageSendRequest {
            target: target.clone(),
            channel: channel.clone(),
            content: content.clone(),
            media_urls: media_urls.clone(),
        })
        .await?;

        // Return structured JSON so the messaging tracker can parse it,
        // while remaining human-readable for the LLM.
        let channel_name = channel.as_deref().unwrap_or("unknown");
        Ok(serde_json::json!({
            "status": "delivered",
            "channel": channel_name,
            "target": target,
            "delivery_id": delivery_id,
            "message": format!("Message delivered to {} successfully.", channel_name)
        }).to_string())
    }
}

// ─── Op8: Live Workspace Awareness ──────────────────────────────────────────

/// Tool that searches the workspace for files matching a glob pattern.
///
/// Returns file paths relative to the workspace root, confined by
/// [`WorkspaceGuard`] so the agent cannot escape the sandbox.
pub struct WorkspaceSearchTool {
    /// `(glob_pattern, max_results) → Ok(json_array_of_paths)`
    search_fn: Arc<
        dyn Fn(
                String,
                usize,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
}

impl WorkspaceSearchTool {
    pub fn new(
        search_fn: Arc<
            dyn Fn(
                    String,
                    usize,
                )
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                + Send
                + Sync,
        >,
    ) -> Self {
        Self { search_fn }
    }
}

#[async_trait]
impl Tool for WorkspaceSearchTool {
    fn name(&self) -> &str {
        "workspace_search"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "workspace_search".into(),
            description: "Search the workspace for files matching a glob pattern. Returns paths \
                relative to the workspace root. Supports standard glob syntax: * (any chars), \
                ** (recursive), ? (single char), [abc] (character class)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match files, e.g. '**/*.rs', 'src/**/*.ts', '*.toml'"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 100, max: 1000)"
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::FileSystem]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        debug!("WorkspaceSearchTool executing with args: {:?}", args);

        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "pattern is required".to_string())?
            .to_string();

        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(100)
            .min(1000) as usize;

        (self.search_fn)(pattern, max_results).await
    }
}

/// Tool that searches file contents within the workspace for a text pattern.
///
/// Returns matching file paths with line numbers and context, confined by
/// [`WorkspaceGuard`].
pub struct WorkspaceGrepTool {
    /// Typed async port for workspace grep.
    grep_fn: crate::port::AsyncPort<crate::port::WorkspaceGrepRequest, Result<String, String>>,
}

impl WorkspaceGrepTool {
    pub fn new(
        grep_fn: crate::port::AsyncPort<crate::port::WorkspaceGrepRequest, Result<String, String>>,
    ) -> Self {
        Self { grep_fn }
    }
}

#[async_trait]
impl Tool for WorkspaceGrepTool {
    fn name(&self) -> &str {
        "workspace_grep"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "workspace_grep".into(),
            description: "Search file contents within the workspace for a text or regex pattern. \
                Returns matching file paths, line numbers, and surrounding context lines."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Text string or regex pattern to search for"
                    },
                    "is_regex": {
                        "type": "boolean",
                        "description": "Whether the query is a regex pattern (default: false, plain text)"
                    },
                    "include_pattern": {
                        "type": "string",
                        "description": "Optional glob pattern to limit which files are searched (e.g. '**/*.rs')"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of matching lines to return (default: 50, max: 500)"
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::FileSystem]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        debug!("WorkspaceGrepTool executing with args: {:?}", args);

        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "query is required".to_string())?
            .to_string();

        let is_regex = args
            .get("is_regex")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let include_pattern = args
            .get("include_pattern")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(50)
            .min(500) as usize;

        (self.grep_fn)(crate::port::WorkspaceGrepRequest {
            query, is_regex, include_glob: include_pattern, max_results,
        }).await
    }
}

/// Register the workspace_search (glob) and workspace_grep (text search) tools.
///
/// - `search_fn`: `(glob_pattern, max_results) → Ok(json_paths)`
/// - `grep_fn`: typed `WorkspaceGrepRequest` port
pub fn register_workspace_tools(
    registry: &mut crate::tools::ToolRegistry,
    search_fn: Arc<
        dyn Fn(
                String,
                usize,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
    grep_fn: crate::port::AsyncPort<crate::port::WorkspaceGrepRequest, Result<String, String>>,
) {
    use std::sync::Arc;
    registry.register(Arc::new(WorkspaceSearchTool::new(search_fn)));
    registry.register(Arc::new(WorkspaceGrepTool::new(grep_fn)));
}

// ─── Op12: Long-Running Durable Tasks ───────────────────────────────────────

/// Tool that manages long-running durable tasks (builds, deployments, pipelines)
/// with checkpoint/resume capability.
///
/// Operations:
/// - **create**: Register a new long-running task with durable storage
/// - **status**: Query a task's current state, progress, and checkpoint info
/// - **checkpoint**: Save the current task state for resume after restart
/// - **resume**: Resume a checkpointed task from the last completed step
/// - **cancel**: Cancel a running task durably
pub struct DurableTaskTool {
    /// `(operation, task_args_json) → Ok(result_json)`
    task_fn: Arc<
        dyn Fn(
                String,
                serde_json::Value,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
}

impl DurableTaskTool {
    pub fn new(
        task_fn: Arc<
            dyn Fn(
                    String,
                    serde_json::Value,
                )
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                + Send
                + Sync,
        >,
    ) -> Self {
        Self { task_fn }
    }
}

#[async_trait]
impl Tool for DurableTaskTool {
    fn name(&self) -> &str {
        "durable_task"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "durable_task".into(),
            description: "Manage long-running durable tasks that survive process restarts. \
                Supports creating tasks, querying status, saving checkpoints for resume, \
                and canceling. Use for builds, deployments, multi-step pipelines, and any \
                work that may outlive the current session."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["create", "status", "checkpoint", "resume", "cancel", "list"],
                        "description": "Operation to perform"
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Task ID (required for status, checkpoint, resume, cancel)"
                    },
                    "name": {
                        "type": "string",
                        "description": "Human-readable task name (for create)"
                    },
                    "executor_agent": {
                        "type": "string",
                        "description": "Agent ID to execute the task (for create)"
                    },
                    "input": {
                        "type": "object",
                        "description": "Input payload for the task (for create/resume)"
                    },
                    "label": {
                        "type": "string",
                        "description": "Checkpoint label (for checkpoint, e.g. 'after step 3')"
                    },
                    "step_outputs": {
                        "type": "object",
                        "description": "Intermediate step outputs to save (for checkpoint)"
                    },
                    "completed_steps": {
                        "type": "integer",
                        "description": "Number of completed steps (for checkpoint)"
                    }
                },
                "required": ["operation"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![] // Task management is an orchestration primitive
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        debug!("DurableTaskTool executing with args: {:?}", args);

        let operation = args
            .get("operation")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "operation is required".to_string())?
            .to_string();

        match operation.as_str() {
            "create" => {
                if args.get("executor_agent").and_then(|v| v.as_str()).is_none() {
                    return Err("executor_agent is required for create".into());
                }
            }
            "status" | "checkpoint" | "resume" | "cancel" => {
                if args.get("task_id").and_then(|v| v.as_str()).is_none() {
                    return Err(format!("task_id is required for {}", operation));
                }
            }
            "list" => {}
            _ => return Err(format!("unknown operation: {}", operation)),
        }

        (self.task_fn)(operation, args).await
    }
}

/// Register the durable_task tool with an async callback.
///
/// The callback receives `(operation, args_json)` and should route to the
/// appropriate task management operation (create/status/checkpoint/resume/cancel).
pub fn register_durable_task_tool(
    registry: &mut crate::tools::ToolRegistry,
    task_fn: Arc<
        dyn Fn(
                String,
                serde_json::Value,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
) {
    use std::sync::Arc;
    registry.register(Arc::new(DurableTaskTool::new(task_fn)));
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
    registry.register(Arc::new(FileEditTool::new(workspace.clone())));
    registry.register(Arc::new(FileListTool::new(workspace.clone())));
    registry.register(Arc::new(GrepTool::new(workspace.clone())));
    registry.register(Arc::new(WebSearchTool::new()));

    // Register persistent process manager tools
    // ProcessStartTool gets workspace so servers default to serving workspace files
    let process_mgr = Arc::new(crate::process_manager::ProcessManager::default());
    registry.register(Arc::new(
        ProcessStartTool::new(Arc::clone(&process_mgr)).with_workspace(workspace)
    ));
    registry.register(Arc::new(ProcessPollTool::new(Arc::clone(&process_mgr))));
    registry.register(Arc::new(ProcessWriteTool::new(Arc::clone(&process_mgr))));
    registry.register(Arc::new(ProcessKillTool::new(Arc::clone(&process_mgr))));
    registry.register(Arc::new(ProcessListTool::new(process_mgr)));

    // MemorySearchTool is registered separately via register_memory_tool()
    // because it needs a callback to the MemoryManager.
    // MessageSendTool is registered separately via register_messaging_tool()
    // because it needs a callback to the channel gateway.
}

// ─── Sub-Agent Tool ──────────────────────────────────────────────────

/// Tool that allows an agent to spawn a sub-agent for delegation.
///
/// The parent agent can delegate a task to another agent, wait for the result,
/// and incorporate it into its own response. Uses the `SpawnConfig` types from
/// `crate::subagent`.
pub struct SpawnSubAgentTool {
    /// Typed async port for sub-agent spawning.
    spawn_fn: crate::port::AsyncPort<crate::port::SpawnSubAgentRequest, Result<String, String>>,
}

impl SpawnSubAgentTool {
    pub fn new(
        spawn_fn: crate::port::AsyncPort<crate::port::SpawnSubAgentRequest, Result<String, String>>,
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
                        "description": "Maximum seconds to wait for the sub-agent (default: 300).",
                        "default": 300
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
            .unwrap_or(300);

        // Recursion depth check — prevent runaway sub-agent spawning.
        if let Err(e) = crate::recursion_depth::check_depth() {
            return Err(format!("sub-agent spawn blocked: {}", e));
        }
        crate::recursion_depth::with_incremented_depth(
            (self.spawn_fn)(crate::port::SpawnSubAgentRequest {
                agent_id, task, timeout_secs,
            }),
        ).await
    }
}

/// Register the sub-agent spawn tool with an async spawn callback.
///
/// The callback receives `(agent_id, task, timeout_secs)` and returns
/// `Ok(response_text)` from the sub-agent or `Err(error_message)`.
pub fn register_subagent_tool(
    registry: &mut crate::tools::ToolRegistry,
    spawn_fn: crate::port::AsyncPort<crate::port::SpawnSubAgentRequest, Result<String, String>>,
) {
    registry.register(Arc::new(SpawnSubAgentTool::new(spawn_fn)));
}

/// Register the messaging tool with a typed async port.
///
/// `available_channels` is the list of actually-connected channel names
/// (e.g., `["telegram", "discord"]`) — embedded in the tool schema so the
/// LLM only sees channels that actually exist.
pub fn register_messaging_tool(
    registry: &mut crate::tools::ToolRegistry,
    send_fn: crate::port::AsyncPort<crate::port::MessageSendRequest, Result<String, String>>,
    available_channels: Vec<String>,
) {
    use std::sync::Arc;
    registry.register(Arc::new(MessageSendTool::new(send_fn, available_channels)));
}

// ─── Op6: Cross-Channel Notification Routing ────────────────────────────────

/// Priority level for notification routing. Higher priorities may trigger
/// additional channels (e.g., critical → SMS + Slack) or different UX
/// (e.g., @here mention in Slack).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationPriority {
    Info,
    Warning,
    Critical,
}

impl NotificationPriority {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

/// Tool that sends a notification to one or more channels simultaneously
/// (fan-out), with optional priority and named target (e.g. `#ops`, `@user`).
///
/// Unlike [`MessageSendTool`] which sends a conversational message to a
/// single channel, this tool is designed for operational notifications that
/// may need to reach the user on multiple platforms at once.
pub struct SendNotificationTool {
    /// Typed async port for fan-out notification.
    notify_fn: crate::port::AsyncPort<crate::port::SendNotificationRequest, Result<String, String>>,
    /// Connected channel names, embedded into the schema for LLM visibility.
    available_channels: Vec<String>,
}

impl SendNotificationTool {
    pub fn new(
        notify_fn: crate::port::AsyncPort<crate::port::SendNotificationRequest, Result<String, String>>,
        available_channels: Vec<String>,
    ) -> Self {
        Self {
            notify_fn,
            available_channels,
        }
    }
}

#[async_trait]
impl Tool for SendNotificationTool {
    fn name(&self) -> &str {
        "send_notification"
    }

    fn schema(&self) -> ToolSchema {
        let channel_enum: Vec<serde_json::Value> = self
            .available_channels
            .iter()
            .map(|c| json!(c))
            .collect();

        ToolSchema {
            name: "send_notification".into(),
            description: "Send a notification to one or more channels simultaneously. \
                Supports priority levels and named targets (e.g. #channel-name, @user). \
                Use this for operational alerts, status updates, and cross-channel fan-out \
                rather than conversational messages."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "channels": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": channel_enum,
                        },
                        "description": "Target channels to notify (fan-out to all listed)"
                    },
                    "message": {
                        "type": "string",
                        "description": "Notification body text"
                    },
                    "target": {
                        "type": "string",
                        "description": "Channel-specific target — e.g. '#ops' for a Slack channel, '@alice' for a DM. If omitted, uses the default destination for each channel."
                    },
                    "priority": {
                        "type": "string",
                        "enum": ["info", "warning", "critical"],
                        "description": "Notification priority (default: info). Critical may trigger additional delivery channels or @here mentions."
                    }
                },
                "required": ["channels", "message"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Messaging]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        debug!("SendNotificationTool executing with args: {:?}", args);

        let channels: Vec<String> = args
            .get("channels")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        if channels.is_empty() {
            return Err("channels array must not be empty".into());
        }

        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "message is required".to_string())?
            .to_string();

        let target = args
            .get("target")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let priority = args
            .get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("info")
            .to_string();

        (self.notify_fn)(crate::port::SendNotificationRequest {
            channels, target, message, priority,
        }).await
    }
}

/// Register the send_notification tool with a typed async port.
pub fn register_notification_tool(
    registry: &mut crate::tools::ToolRegistry,
    notify_fn: crate::port::AsyncPort<crate::port::SendNotificationRequest, Result<String, String>>,
    available_channels: Vec<String>,
) {
    use std::sync::Arc;
    registry.register(Arc::new(SendNotificationTool::new(
        notify_fn,
        available_channels,
    )));
}

// ─── Op7: Dynamic MCP Server Connection ─────────────────────────────────────

/// Tool that lets the LLM connect to an MCP server at runtime and discover
/// its available tools.
///
/// The response lists discovered tools with their schemas so the LLM can
/// subsequently invoke them via [`McpCallTool`].
pub struct McpConnectTool {
    /// `McpConnectRequest → Ok(discovered_tools_json)`
    connect_fn: crate::port::AsyncPort<crate::port::McpConnectRequest, Result<String, String>>,
}

impl McpConnectTool {
    pub fn new(
        connect_fn: crate::port::AsyncPort<crate::port::McpConnectRequest, Result<String, String>>,
    ) -> Self {
        Self { connect_fn }
    }
}

#[async_trait]
impl Tool for McpConnectTool {
    fn name(&self) -> &str {
        "mcp_connect"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "mcp_connect".into(),
            description: "Connect to an MCP (Model Context Protocol) server and discover its \
                available tools. After connecting, use mcp_call to invoke discovered tools. \
                Supports stdio (local command) and SSE (HTTP URL) transports."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "server_name": {
                        "type": "string",
                        "description": "A unique name for this server connection (used to reference it later in mcp_call)"
                    },
                    "transport": {
                        "type": "string",
                        "enum": ["stdio", "sse"],
                        "description": "Transport type: 'stdio' for local commands, 'sse' for HTTP SSE endpoints"
                    },
                    "command_or_url": {
                        "type": "string",
                        "description": "For stdio: the command to run (e.g. 'npx'). For sse: the server URL."
                    },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Command arguments for stdio transport (e.g. ['-y', '@modelcontextprotocol/server-github']). Ignored for SSE."
                    }
                },
                "required": ["server_name", "transport", "command_or_url"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ExternalApi]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        debug!("McpConnectTool executing with args: {:?}", args);

        let server_name = args
            .get("server_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "server_name is required".to_string())?
            .to_string();

        let transport = args
            .get("transport")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "transport is required (stdio or sse)".to_string())?
            .to_string();

        let command_or_url = args
            .get("command_or_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "command_or_url is required".to_string())?
            .to_string();

        let cmd_args: Vec<String> = args
            .get("args")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        (self.connect_fn)(crate::port::McpConnectRequest {
            server_name, transport, command_or_url, args: cmd_args,
        }).await
    }
}

/// Tool that calls a tool on a previously-connected MCP server.
///
/// Works in tandem with [`McpConnectTool`]: first connect to discover tools,
/// then use this to invoke them by name.
pub struct McpCallTool {
    /// `(server_name, tool_name, arguments_json) → Ok(result_text)`
    call_fn: Arc<
        dyn Fn(
                String,
                String,
                serde_json::Value,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
}

impl McpCallTool {
    pub fn new(
        call_fn: Arc<
            dyn Fn(
                    String,
                    String,
                    serde_json::Value,
                )
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                + Send
                + Sync,
        >,
    ) -> Self {
        Self { call_fn }
    }
}

#[async_trait]
impl Tool for McpCallTool {
    fn name(&self) -> &str {
        "mcp_call"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "mcp_call".into(),
            description: "Call a tool on a connected MCP server. Use mcp_connect first to \
                connect and discover available tools, then call them by server name and tool name."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "server": {
                        "type": "string",
                        "description": "The server_name used when connecting via mcp_connect"
                    },
                    "tool": {
                        "type": "string",
                        "description": "The tool name as returned by mcp_connect"
                    },
                    "arguments": {
                        "type": "object",
                        "description": "Tool arguments matching the schema returned by mcp_connect"
                    }
                },
                "required": ["server", "tool"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ExternalApi]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        debug!("McpCallTool executing with args: {:?}", args);

        let server = args
            .get("server")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "server is required".to_string())?
            .to_string();

        let tool = args
            .get("tool")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "tool is required".to_string())?
            .to_string();

        let arguments = args
            .get("arguments")
            .cloned()
            .unwrap_or(json!({}));

        (self.call_fn)(server, tool, arguments).await
    }
}

/// Register the MCP connect + call tools with async callbacks.
///
/// - `connect_fn`: typed `McpConnectRequest` port
/// - `call_fn`: `(server, tool_name, arguments) → Ok(result_text)`
pub fn register_mcp_tools(
    registry: &mut crate::tools::ToolRegistry,
    connect_fn: crate::port::AsyncPort<crate::port::McpConnectRequest, Result<String, String>>,
    call_fn: Arc<
        dyn Fn(
                String,
                String,
                serde_json::Value,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
) {
    use std::sync::Arc;
    registry.register(Arc::new(McpConnectTool::new(connect_fn)));
    registry.register(Arc::new(McpCallTool::new(call_fn)));
}

// ─── Op3: Pipeline Composition ──────────────────────────────────────────────

/// Tool that lets the LLM compose and optionally execute agent pipelines.
///
/// A pipeline is a directed graph of named stages: sequential agent calls,
/// parallel branches with merge strategies, conditional routing gates, and
/// text transforms. This tool bridges the [`AgentPipeline`] infrastructure
/// to the agent's tool-calling surface.
pub struct PipelineComposeTool {
    /// `(pipeline_json) → Ok(pipeline_id_or_result)`
    compose_fn: Arc<
        dyn Fn(
                serde_json::Value,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
}

impl PipelineComposeTool {
    pub fn new(
        compose_fn: Arc<
            dyn Fn(
                    serde_json::Value,
                )
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                + Send
                + Sync,
        >,
    ) -> Self {
        Self { compose_fn }
    }
}

#[async_trait]
impl Tool for PipelineComposeTool {
    fn name(&self) -> &str {
        "compose_pipeline"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "compose_pipeline".into(),
            description: "Build and optionally execute a multi-agent pipeline. Define a sequence \
                of stages (agent calls, parallel branches, routing gates, transforms) that process \
                input through a DAG. Use this for complex multi-step workflows that coordinate \
                multiple agents."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Human-readable pipeline name"
                    },
                    "stages": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "type": {
                                    "type": "string",
                                    "enum": ["agent", "transform", "gate", "parallel"],
                                    "description": "Stage type: agent (invoke an agent), transform (apply text operation), gate (conditional branch), parallel (fan-out)"
                                },
                                "agent_id": {
                                    "type": "string",
                                    "description": "Agent ID for 'agent' stages"
                                },
                                "expression": {
                                    "type": "string",
                                    "description": "For 'transform': a Jinja-like template. For 'gate': a condition expression (e.g. 'contains:error')"
                                },
                                "branches": {
                                    "type": "array",
                                    "items": { "type": "object" },
                                    "description": "Sub-stages for parallel execution"
                                },
                                "merge": {
                                    "type": "string",
                                    "enum": ["concat", "structured", "first_success", "best"],
                                    "description": "Merge strategy for parallel branches (default: concat)"
                                },
                                "timeout_secs": {
                                    "type": "integer",
                                    "description": "Per-stage timeout in seconds (default: 60)"
                                }
                            },
                            "required": ["type"]
                        },
                        "description": "Ordered list of pipeline stages to execute sequentially"
                    },
                    "error_policy": {
                        "type": "string",
                        "enum": ["fail_fast", "continue_on_error", "retry"],
                        "description": "Error handling policy (default: fail_fast)"
                    },
                    "run_immediately": {
                        "type": "boolean",
                        "description": "If true, execute the pipeline now with the provided input. If false, save for later execution."
                    },
                    "input": {
                        "type": "string",
                        "description": "Input text to feed into the pipeline (required if run_immediately is true)"
                    }
                },
                "required": ["name", "stages"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![] // Pipeline composition is an orchestration primitive
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        debug!("PipelineComposeTool executing with args: {:?}", args);

        // Validate required fields
        if args.get("name").and_then(|v| v.as_str()).is_none() {
            return Err("name is required".into());
        }
        if args.get("stages").and_then(|v| v.as_array()).map_or(true, |a| a.is_empty()) {
            return Err("stages array must not be empty".into());
        }
        if args.get("run_immediately").and_then(|v| v.as_bool()).unwrap_or(false) {
            if args.get("input").and_then(|v| v.as_str()).is_none() {
                return Err("input is required when run_immediately is true".into());
            }
        }

        (self.compose_fn)(args).await
    }
}

/// Register the compose_pipeline tool with an async callback.
///
/// The callback receives the full pipeline JSON definition and should
/// construct an `AgentPipeline`, validate it, and either persist it or
/// execute it immediately via `PipelineExecutor`.
pub fn register_pipeline_compose_tool(
    registry: &mut crate::tools::ToolRegistry,
    compose_fn: Arc<
        dyn Fn(
                serde_json::Value,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
) {
    use std::sync::Arc;
    registry.register(Arc::new(PipelineComposeTool::new(compose_fn)));
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

/// Register the memory forget tool with an async forget callback.
///
/// The callback receives a `memory_id` string and should delete the entry via
/// `MemoryManager::forget()`.
pub fn register_memory_forget_tool_async(
    registry: &mut crate::tools::ToolRegistry,
    forget_fn: std::sync::Arc<
        dyn Fn(
                String,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
            + Send
            + Sync,
    >,
) {
    use std::sync::Arc;
    registry.register(Arc::new(MemoryForgetTool::with_async_forget(forget_fn)));
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
    /// Typed async port for A2A session messaging.
    send_fn: crate::port::AsyncPort<crate::port::SessionsSendRequest, Result<String, String>>,
}

impl SessionsSendTool {
    pub fn new(
        send_fn: crate::port::AsyncPort<crate::port::SessionsSendRequest, Result<String, String>>,
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

        (self.send_fn)(crate::port::SessionsSendRequest {
            target_agent, message, skill_id,
        }).await
    }
}

/// Register the A2A sessions_send tool with a typed async port.
pub fn register_sessions_send_tool(
    registry: &mut crate::tools::ToolRegistry,
    send_fn: crate::port::AsyncPort<crate::port::SessionsSendRequest, Result<String, String>>,
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

// ─── Op5: Agent Discovery for Delegation ────────────────────────────────────

/// Tool that queries the ACP agent directory for agents matching required
/// capabilities, returning ranked candidates with scores.
///
/// Designed as the "discovery → delegate" companion to [`SessionsSendTool`] and
/// [`SpawnSubAgentTool`]: the LLM first calls `discover_agents` to find
/// candidates, then picks one and delegates via `sessions_send` or
/// `spawn_subagent`.
pub struct DiscoverAgentsTool {
    /// `(capability_names, min_score) → JSON array of matching agents`
    discover_fn: Arc<
        dyn Fn(
                Vec<String>,
                f64,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
}

impl DiscoverAgentsTool {
    pub fn new(
        discover_fn: Arc<
            dyn Fn(
                    Vec<String>,
                    f64,
                )
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                + Send
                + Sync,
        >,
    ) -> Self {
        Self { discover_fn }
    }
}

#[async_trait]
impl Tool for DiscoverAgentsTool {
    fn name(&self) -> &str {
        "discover_agents"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "discover_agents".into(),
            description: "Search the agent directory for agents matching required capabilities. \
                Returns ranked candidates with scores. Use this before sessions_send or \
                spawn_subagent to find the best agent for a task."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "capabilities": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Required capability names, e.g. ['web_search', 'code_execution']. \
                            Uses hierarchical matching: requesting 'media_processing' also matches \
                            agents with 'image_processing', 'audio_processing', etc."
                    },
                    "min_score": {
                        "type": "number",
                        "description": "Minimum capability match score between 0.0 and 1.0 (default: 0.5). \
                            A score of 1.0 means all requested capabilities are present."
                    }
                },
                "required": ["capabilities"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![] // Read-only directory query
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        debug!("DiscoverAgentsTool executing with args: {:?}", args);

        let capabilities: Vec<String> = args
            .get("capabilities")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        if capabilities.is_empty() {
            return Err("capabilities array must not be empty".into());
        }

        let min_score = args
            .get("min_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.5)
            .clamp(0.0, 1.0);

        (self.discover_fn)(capabilities, min_score).await
    }
}

/// Register the discover_agents tool with an async callback.
///
/// The callback receives `(capability_names, min_score)` and returns a JSON
/// string with matching agents ranked by score.
pub fn register_discover_agents_tool(
    registry: &mut crate::tools::ToolRegistry,
    discover_fn: Arc<
        dyn Fn(
                Vec<String>,
                f64,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
) {
    registry.register(Arc::new(DiscoverAgentsTool::new(discover_fn)));
}

// ─── Dynamic Agent Spawning ─────────────────────────────────────────

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
const DEFAULT_DYNAMIC_TIMEOUT_SECS: u64 = 300;
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

/// Register the dynamic agent spawn tool.
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

// ─── Op1: Browser Action Tools ──────────────────────────────────────────────

/// Browser integration tool — dispatches LLM tool calls to a browser backend.
///
/// Uses the callback pattern (like `MessageSendTool`) because the actual
/// `CdpSession` is managed externally. The `execute_fn` callback receives
/// `(tool_name, json_args)` and returns the result text or error.
///
/// Seven instances are created, one per browser tool definition, all sharing
/// the same callback closure.
pub struct BrowserActionTool {
    tool_name: String,
    description: String,
    parameters: serde_json::Value,
    /// Callback: `(tool_name, args_json) → Result<output_text, error_msg>`.
    execute_fn: Arc<
        dyn Fn(
                String,
                serde_json::Value,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
}

impl BrowserActionTool {
    /// Create a browser tool for a specific action.
    pub fn new(
        tool_name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        execute_fn: Arc<
            dyn Fn(
                    String,
                    serde_json::Value,
                )
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                + Send
                + Sync,
        >,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            description: description.into(),
            parameters,
            execute_fn,
        }
    }
}

#[async_trait::async_trait]
impl crate::tools::Tool for BrowserActionTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: self.tool_name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }

    fn required_capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::Browser]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        (self.execute_fn)(self.tool_name.clone(), args).await
    }
}

/// Browser tool definitions for LLM function-calling integration.
///
/// Returns `(name, description, json_schema_parameters)` tuples for the 7
/// browser actions. Defined here so `clawdesk-agents` doesn't need to depend
/// on `clawdesk-browser`.
fn browser_tool_definitions() -> Vec<(&'static str, &'static str, serde_json::Value)> {
    vec![
        (
            "browser_navigate",
            "Navigate to a URL in the browser",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to navigate to"
                    }
                },
                "required": ["url"]
            }),
        ),
        (
            "browser_click",
            "Click an element by CSS selector",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "selector": {
                        "type": "string",
                        "description": "CSS selector for the element to click"
                    }
                },
                "required": ["selector"]
            }),
        ),
        (
            "browser_type",
            "Type text into an input element",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "selector": {
                        "type": "string",
                        "description": "CSS selector for the input element"
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to type into the input"
                    }
                },
                "required": ["selector", "text"]
            }),
        ),
        (
            "browser_screenshot",
            "Take a screenshot of the current page",
            serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        ),
        (
            "browser_extract_text",
            "Extract text from the page or a specific element",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "selector": {
                        "type": "string",
                        "description": "Optional CSS selector to extract text from a specific element"
                    }
                }
            }),
        ),
        (
            "browser_get_title",
            "Get the current page title",
            serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        ),
        (
            "browser_eval_js",
            "Execute JavaScript on the page and return the result",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "expression": {
                        "type": "string",
                        "description": "JavaScript expression to evaluate"
                    }
                },
                "required": ["expression"]
            }),
        ),
    ]
}

/// Register all 7 browser action tools into the tool registry.
///
/// The `execute_fn` callback is shared across all browser tools. It receives
/// `(tool_name, json_args)` and should dispatch to the actual browser backend
/// (e.g., via `clawdesk_browser::execute_tool_call`).
///
/// ## Example (in `clawdesk-tauri` state init)
///
/// ```ignore
/// let session = Arc::new(tokio::sync::Mutex::new(cdp_session));
/// let execute_fn = Arc::new(move |name: String, args: serde_json::Value| {
///     let session = session.clone();
///     Box::pin(async move {
///         let s = session.lock().await;
///         clawdesk_browser::execute_tool_call(&s, &name, &args).await
///     }) as Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
/// });
/// register_browser_tools(&mut registry, execute_fn);
/// ```
pub fn register_browser_tools(
    registry: &mut crate::tools::ToolRegistry,
    execute_fn: Arc<
        dyn Fn(
                String,
                serde_json::Value,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
) {
    use std::sync::Arc;

    for (name, desc, params) in browser_tool_definitions() {
        registry.register(Arc::new(BrowserActionTool::new(
            name,
            desc,
            params,
            execute_fn.clone(),
        )));
    }
}

// ─── Op2: Cron Management Tools ─────────────────────────────────────────────

/// Schedule a recurring task via the cron manager.
///
/// Uses typed [`CronScheduleRequest`] instead of 6 positional args.
pub struct CronScheduleTool {
    schedule_fn: crate::port::AsyncPort<crate::port::CronScheduleRequest, Result<String, String>>,
}

impl CronScheduleTool {
    pub fn new(
        schedule_fn: crate::port::AsyncPort<crate::port::CronScheduleRequest, Result<String, String>>,
    ) -> Self {
        Self { schedule_fn }
    }
}

#[async_trait::async_trait]
impl crate::tools::Tool for CronScheduleTool {
    fn name(&self) -> &str { "cron_schedule" }

    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "cron_schedule".to_string(),
            description: "Schedule a recurring task. Creates or updates a cron job that \
                         runs an agent prompt on a schedule."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Human-readable task name" },
                    "schedule": { "type": "string", "description": "Cron expression (5-field: '*/5 * * * *' = every 5 min)" },
                    "prompt": { "type": "string", "description": "The prompt to execute on each scheduled run" },
                    "agent_id": { "type": "string", "description": "Target agent ID (optional, defaults to current agent)" },
                    "timeout_secs": { "type": "integer", "description": "Max execution time in seconds", "default": 300 },
                    "task_id": { "type": "string", "description": "Existing task ID to update (omit to create new)" }
                },
                "required": ["name", "schedule", "prompt"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::Scheduling]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let name = args.get("name").and_then(|v| v.as_str())
            .ok_or("Missing required parameter: name")?.to_string();
        let schedule = args.get("schedule").and_then(|v| v.as_str())
            .ok_or("Missing required parameter: schedule")?.to_string();
        let prompt = args.get("prompt").and_then(|v| v.as_str())
            .ok_or("Missing required parameter: prompt")?.to_string();
        let agent_id = args.get("agent_id").and_then(|v| v.as_str()).map(String::from);
        let timeout_secs = args.get("timeout_secs").and_then(|v| v.as_u64()).unwrap_or(300);
        let task_id = args.get("task_id").and_then(|v| v.as_str()).map(String::from);

        (self.schedule_fn)(crate::port::CronScheduleRequest {
            name, schedule, prompt, agent_id, timeout_secs, task_id,
        }).await
    }
}

/// List all scheduled cron tasks.
pub struct CronListTool {
    list_fn: Arc<
        dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send + Sync,
    >,
}

impl CronListTool {
    pub fn new(
        list_fn: Arc<
            dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                + Send + Sync,
        >,
    ) -> Self {
        Self { list_fn }
    }
}

#[async_trait::async_trait]
impl crate::tools::Tool for CronListTool {
    fn name(&self) -> &str { "cron_list" }

    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "cron_list".to_string(),
            description: "List all scheduled cron tasks with their status and schedule.".to_string(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }

    fn required_capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::Scheduling]
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        (self.list_fn)().await
    }
}

/// Remove a scheduled cron task by ID.
pub struct CronRemoveTool {
    remove_fn: Arc<
        dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send + Sync,
    >,
}

impl CronRemoveTool {
    pub fn new(
        remove_fn: Arc<
            dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                + Send + Sync,
        >,
    ) -> Self {
        Self { remove_fn }
    }
}

#[async_trait::async_trait]
impl crate::tools::Tool for CronRemoveTool {
    fn name(&self) -> &str { "cron_remove" }

    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "cron_remove".to_string(),
            description: "Remove/cancel a scheduled cron task by ID.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "ID of the cron task to remove" }
                },
                "required": ["task_id"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::Scheduling]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let task_id = args.get("task_id").and_then(|v| v.as_str())
            .ok_or("Missing required parameter: task_id")?.to_string();
        (self.remove_fn)(task_id).await
    }
}

/// Manually trigger a cron task immediately.
pub struct CronTriggerTool {
    trigger_fn: Arc<
        dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send + Sync,
    >,
}

impl CronTriggerTool {
    pub fn new(
        trigger_fn: Arc<
            dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                + Send + Sync,
        >,
    ) -> Self {
        Self { trigger_fn }
    }
}

#[async_trait::async_trait]
impl crate::tools::Tool for CronTriggerTool {
    fn name(&self) -> &str { "cron_trigger" }

    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "cron_trigger".to_string(),
            description: "Manually trigger a scheduled cron task immediately, ignoring its \
                         schedule. Returns the execution result."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "ID of the cron task to trigger" }
                },
                "required": ["task_id"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::Scheduling]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let task_id = args.get("task_id").and_then(|v| v.as_str())
            .ok_or("Missing required parameter: task_id")?.to_string();
        (self.trigger_fn)(task_id).await
    }
}

/// Register all 4 cron management tools into the tool registry.
///
/// Each callback captures `Arc<CronManager>` and delegates to its methods.
/// Call this during state initialization after constructing the CronManager.
pub fn register_cron_tools(
    registry: &mut crate::tools::ToolRegistry,
    schedule_fn: crate::port::AsyncPort<crate::port::CronScheduleRequest, Result<String, String>>,
    list_fn: Arc<
        dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send + Sync,
    >,
    remove_fn: Arc<
        dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send + Sync,
    >,
    trigger_fn: Arc<
        dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send + Sync,
    >,
) {
    use std::sync::Arc;
    registry.register(Arc::new(CronScheduleTool::new(schedule_fn)));
    registry.register(Arc::new(CronListTool::new(list_fn)));
    registry.register(Arc::new(CronRemoveTool::new(remove_fn)));
    registry.register(Arc::new(CronTriggerTool::new(trigger_fn)));
}

// ─── T10: Persistent Process Manager Tools ──────────────────────────────────

/// Start a persistent background process that can be polled and interacted with.
pub struct ProcessStartTool {
    manager: Arc<crate::process_manager::ProcessManager>,
    workspace: Option<PathBuf>,
}

impl ProcessStartTool {
    pub fn new(manager: Arc<crate::process_manager::ProcessManager>) -> Self {
        Self { manager, workspace: None }
    }

    pub fn with_workspace(mut self, workspace: Option<PathBuf>) -> Self {
        self.workspace = workspace;
        self
    }
}

#[async_trait::async_trait]
impl crate::tools::Tool for ProcessStartTool {
    fn name(&self) -> &str {
        "process_start"
    }

    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "process_start".to_string(),
            description: "Start a persistent background process. Returns a process ID for \
                         polling output and sending input. Use for servers, watch commands, \
                         and interactive tools."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "process_id": {
                        "type": "string",
                        "description": "Unique identifier for this process (e.g., 'dev-server', 'test-runner')."
                    },
                    "command": {
                        "type": "string",
                        "description": "Shell command to run in the background."
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Working directory for the process."
                    }
                },
                "required": ["process_id", "command"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ShellExec]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let process_id = args.get("process_id").and_then(|v| v.as_str())
            .ok_or("Missing required parameter: process_id")?.to_string();
        let command = args.get("command").and_then(|v| v.as_str())
            .ok_or("Missing required parameter: command")?;
        // Resolve working_dir: use provided value relative to workspace,
        // or default to workspace root so servers serve the right files.
        let working_dir_resolved = args.get("working_dir")
            .and_then(|v| v.as_str())
            .map(|d| {
                if let Some(ref ws) = self.workspace {
                    ws.join(d).to_string_lossy().to_string()
                } else {
                    d.to_string()
                }
            })
            .or_else(|| self.workspace.as_ref().map(|ws| ws.to_string_lossy().to_string()));

        // Exec policy check on the command
        {
            use crate::exec_policy::{ExecPolicy, ExecPolicyConfig, ExecVerdict};
            let policy = ExecPolicy::new(ExecPolicyConfig::default());
            if let ExecVerdict::Deny { reason } = policy.check(command) {
                return Err(format!("command blocked by exec policy: {}", reason));
            }
        }

        self.manager
            .start(process_id.clone(), command, working_dir_resolved.as_deref(), None)
            .await?;

        Ok(format!("Process '{}' started. Use process_poll to read output, process_write to send input.", process_id))
    }
}

/// Poll a running process for new stdout/stderr output since last poll.
pub struct ProcessPollTool {
    manager: Arc<crate::process_manager::ProcessManager>,
}

impl ProcessPollTool {
    pub fn new(manager: Arc<crate::process_manager::ProcessManager>) -> Self {
        Self { manager }
    }
}

#[async_trait::async_trait]
impl crate::tools::Tool for ProcessPollTool {
    fn name(&self) -> &str {
        "process_poll"
    }

    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "process_poll".to_string(),
            description: "Read new output from a running process since the last poll. \
                         Returns stdout, stderr, and process status."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "process_id": {
                        "type": "string",
                        "description": "ID of the process to poll."
                    }
                },
                "required": ["process_id"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let process_id = args.get("process_id").and_then(|v| v.as_str())
            .ok_or("Missing required parameter: process_id")?;

        let result = self.manager.poll(process_id).await?;
        Ok(serde_json::json!({
            "stdout": result.stdout,
            "stderr": result.stderr,
            "running": result.running,
            "exit_code": result.exit_code,
            "elapsed_secs": result.elapsed_secs,
        }).to_string())
    }
}

/// Write data to a running process's stdin.
pub struct ProcessWriteTool {
    manager: Arc<crate::process_manager::ProcessManager>,
}

impl ProcessWriteTool {
    pub fn new(manager: Arc<crate::process_manager::ProcessManager>) -> Self {
        Self { manager }
    }
}

#[async_trait::async_trait]
impl crate::tools::Tool for ProcessWriteTool {
    fn name(&self) -> &str {
        "process_write"
    }

    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "process_write".to_string(),
            description: "Write data to a running process's stdin. Use for interactive \
                         programs that need input (e.g., answering prompts, sending commands)."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "process_id": {
                        "type": "string",
                        "description": "ID of the process to write to."
                    },
                    "data": {
                        "type": "string",
                        "description": "Data to write to stdin (a newline is NOT added automatically)."
                    }
                },
                "required": ["process_id", "data"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let process_id = args.get("process_id").and_then(|v| v.as_str())
            .ok_or("Missing required parameter: process_id")?;
        let data = args.get("data").and_then(|v| v.as_str())
            .ok_or("Missing required parameter: data")?;

        self.manager.write(process_id, data).await?;
        Ok(format!("Wrote {} bytes to process '{}'.", data.len(), process_id))
    }
}

/// Kill a running managed process.
pub struct ProcessKillTool {
    manager: Arc<crate::process_manager::ProcessManager>,
}

impl ProcessKillTool {
    pub fn new(manager: Arc<crate::process_manager::ProcessManager>) -> Self {
        Self { manager }
    }
}

#[async_trait::async_trait]
impl crate::tools::Tool for ProcessKillTool {
    fn name(&self) -> &str {
        "process_kill"
    }

    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "process_kill".to_string(),
            description: "Kill a running managed process and remove it from the registry."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "process_id": {
                        "type": "string",
                        "description": "ID of the process to kill."
                    }
                },
                "required": ["process_id"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let process_id = args.get("process_id").and_then(|v| v.as_str())
            .ok_or("Missing required parameter: process_id")?;

        self.manager.kill(process_id).await?;
        Ok(format!("Process '{}' killed.", process_id))
    }
}

/// List all active managed processes.
pub struct ProcessListTool {
    manager: Arc<crate::process_manager::ProcessManager>,
}

impl ProcessListTool {
    pub fn new(manager: Arc<crate::process_manager::ProcessManager>) -> Self {
        Self { manager }
    }
}

#[async_trait::async_trait]
impl crate::tools::Tool for ProcessListTool {
    fn name(&self) -> &str {
        "process_list"
    }

    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "process_list".to_string(),
            description: "List all active managed processes with their status, command, \
                         and resource usage."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        }
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        let statuses = self.manager.list().await;
        if statuses.is_empty() {
            return Ok("No active managed processes.".to_string());
        }
        Ok(serde_json::to_string_pretty(&statuses)
            .unwrap_or_else(|_| "Failed to serialize process list".to_string()))
    }
}

// ─── T7: MCP Namespace Bridge Tool ──────────────────────────────────────────

/// Discovered MCP tool metadata — passed to `register_mcp_bridge_tools()`.
#[derive(Debug, Clone)]
pub struct McpDiscoveredTool {
    /// MCP server name (e.g., "github").
    pub server_name: String,
    /// Original tool name on the MCP server (e.g., "create_issue").
    pub original_name: String,
    /// Tool description from MCP server.
    pub description: String,
    /// Tool input schema from MCP server.
    pub input_schema: serde_json::Value,
}

/// A transparent MCP bridge tool that registers in the ToolRegistry under a
/// namespaced name (e.g., `mcp_github_create_issue`). When the LLM calls it,
/// execute() routes through the MCP client callback.
pub struct McpBridgeToolInstance {
    namespaced_name: String,
    original_name: String,
    server_name: String,
    description: String,
    input_schema: serde_json::Value,
    call_fn: Arc<
        dyn Fn(String, String, serde_json::Value)
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send + Sync,
    >,
}

#[async_trait::async_trait]
impl crate::tools::Tool for McpBridgeToolInstance {
    fn name(&self) -> &str {
        &self.namespaced_name
    }

    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: self.namespaced_name.clone(),
            description: format!(
                "[MCP: {}/{}] {}",
                self.server_name, self.original_name, self.description
            ),
            parameters: self.input_schema.clone(),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        (self.call_fn)(
            self.server_name.clone(),
            self.original_name.clone(),
            args,
        )
        .await
    }
}

/// Register MCP bridge tools into the ToolRegistry.
///
/// Each discovered MCP tool gets a namespaced name (`mcp_{server}_{tool}`)
/// and is registered as a transparent bridge that routes through the MCP client.
/// The `call_fn` callback receives `(server_name, tool_name, arguments)` and
/// dispatches via the MCP protocol.
///
/// Call this during agent initialization after MCP server discovery completes.
pub fn register_mcp_bridge_tools(
    registry: &mut crate::tools::ToolRegistry,
    tools: Vec<McpDiscoveredTool>,
    call_fn: Arc<
        dyn Fn(String, String, serde_json::Value)
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send + Sync,
    >,
) {
    use std::sync::Arc;
    for tool in tools {
        let namespaced = format!(
            "mcp_{}_{}",
            tool.server_name.replace('-', "_").replace(' ', "_").to_lowercase(),
            tool.original_name.replace('-', "_").to_lowercase(),
        );
        info!(
            namespaced = %namespaced,
            server = %tool.server_name,
            tool = %tool.original_name,
            "registering MCP bridge tool"
        );
        registry.register(Arc::new(McpBridgeToolInstance {
            namespaced_name: namespaced,
            original_name: tool.original_name,
            server_name: tool.server_name,
            description: tool.description,
            input_schema: tool.input_schema,
            call_fn: Arc::clone(&call_fn),
        }));
    }
}

// ─── Ask Human Tool ──────────────────────────────────────────────────

/// Tool that pauses agent execution and asks the human for a decision.
///
/// Instead of refusing or restating capabilities, the agent calls this tool
/// to present a question with optional choices. The tool blocks until the
/// human responds, then returns their answer as the tool result so the
/// agent can proceed accordingly.
pub struct AskHumanTool {
    ask_fn: crate::port::AsyncPort<crate::port::AskHumanRequest, Result<String, String>>,
}

impl AskHumanTool {
    pub fn new(
        ask_fn: crate::port::AsyncPort<crate::port::AskHumanRequest, Result<String, String>>,
    ) -> Self {
        Self { ask_fn }
    }
}

#[async_trait]
impl Tool for AskHumanTool {
    fn name(&self) -> &str {
        "ask_human"
    }

    fn schema(&self) -> crate::tools::ToolSchema {
        crate::tools::ToolSchema {
            name: "ask_human".into(),
            description: "Ask the human user for a decision, confirmation, or input before proceeding. \
                Use this when you need the user's approval, preference, or choice — instead of refusing \
                or just listing options. The tool blocks until the user responds and returns their answer."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question or decision to present to the user. Be specific about what you need from them."
                    },
                    "options": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of suggested choices. If empty, the user gives a free-form answer."
                    },
                    "urgent": {
                        "type": "boolean",
                        "description": "Whether this needs immediate attention (true) or is a non-blocking preference question (false)."
                    }
                },
                "required": ["question"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let question = args
            .get("question")
            .and_then(|v| v.as_str())
            .ok_or("missing 'question' argument")?
            .to_string();

        let options: Vec<String> = args
            .get("options")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let urgent = args
            .get("urgent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let request = crate::port::AskHumanRequest {
            question,
            options,
            urgent,
        };

        (self.ask_fn)(request).await
    }
}

/// Register the `ask_human` tool with the given callback for soliciting user input.
pub fn register_ask_human_tool(
    registry: &mut crate::tools::ToolRegistry,
    ask_fn: crate::port::AsyncPort<crate::port::AskHumanRequest, Result<String, String>>,
) {
    use std::sync::Arc;
    registry.register(Arc::new(AskHumanTool::new(ask_fn)));
}
