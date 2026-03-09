//! MCP transport layer — Stdio and SSE transports.

use crate::{JsonRpcRequest, JsonRpcResponse, McpError};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, error, warn};

/// Transport trait for MCP communication.
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Send a request and receive a response.
    async fn send_request(&self, request: JsonRpcRequest) -> Result<JsonRpcResponse, McpError>;

    /// Send a notification (no response expected).
    async fn send_notification(&self, notification: JsonRpcRequest) -> Result<(), McpError>;

    /// Close the transport.
    async fn close(&self) -> Result<(), McpError>;
}

// ---------------------------------------------------------------------------
// Stdio Transport
// ---------------------------------------------------------------------------

/// Stdio transport — communicates with MCP server via subprocess stdin/stdout.
pub struct StdioTransport {
    /// Writer to subprocess stdin
    writer: Mutex<ChildStdin>,
    /// Pending request map: id → response sender
    pending: Arc<dashmap::DashMap<u64, oneshot::Sender<JsonRpcResponse>>>,
    /// Monotonic request ID counter
    next_id: AtomicU64,
    /// Background reader task handle
    _reader_handle: tokio::task::JoinHandle<()>,
    /// Child process handle for cleanup
    child: Mutex<Option<Child>>,
}

impl StdioTransport {
    /// Spawn a subprocess and create a stdio transport.
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self, McpError> {
        let mut cmd = Command::new(command);
        cmd.args(args);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        // Set environment
        for (key, value) in env {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn().map_err(|e| {
            McpError::Transport(format!("failed to spawn MCP server '{}': {}", command, e))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("no stdin on child process".into()))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("no stdout on child process".into()))?;

        let pending: Arc<dashmap::DashMap<u64, oneshot::Sender<JsonRpcResponse>>> =
            Arc::new(dashmap::DashMap::new());

        // Spawn background reader
        let reader_pending = Arc::clone(&pending);
        let reader_handle = tokio::spawn(async move {
            Self::reader_loop(stdout, reader_pending).await;
        });

        Ok(Self {
            writer: Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            _reader_handle: reader_handle,
            child: Mutex::new(Some(child)),
        })
    }

    /// Background loop reading JSON-RPC responses from stdout.
    async fn reader_loop(
        stdout: ChildStdout,
        pending: Arc<dashmap::DashMap<u64, oneshot::Sender<JsonRpcResponse>>>,
    ) {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();

        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }

                    match serde_json::from_str::<JsonRpcResponse>(&line) {
                        Ok(response) => {
                            if let Some(id) = &response.id {
                                if let Some(id_num) = id.as_u64() {
                                    if let Some((_, sender)) = pending.remove(&id_num) {
                                        let _ = sender.send(response);
                                    } else {
                                        debug!(id = id_num, "response for unknown request id");
                                    }
                                }
                            }
                            // Notifications (id=null) are logged but not dispatched
                        }
                        Err(e) => {
                            warn!(line = %line, error = %e, "failed to parse MCP response");
                        }
                    }
                }
                Ok(None) => {
                    debug!("MCP server stdout closed");
                    break;
                }
                Err(e) => {
                    error!(error = %e, "error reading MCP server stdout");
                    break;
                }
            }
        }
    }

    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn send_request(&self, mut request: JsonRpcRequest) -> Result<JsonRpcResponse, McpError> {
        let id = self.next_request_id();
        request.id = Some(serde_json::Value::Number(id.into()));

        // Register pending response
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);

        // Serialize and send
        let line = serde_json::to_string(&request)?;
        {
            let mut writer = self.writer.lock().await;
            writer
                .write_all(line.as_bytes())
                .await
                .map_err(|e| McpError::Transport(format!("write failed: {}", e)))?;
            writer
                .write_all(b"\n")
                .await
                .map_err(|e| McpError::Transport(format!("write newline failed: {}", e)))?;
            writer
                .flush()
                .await
                .map_err(|e| McpError::Transport(format!("flush failed: {}", e)))?;
        }

        // Wait for response with timeout
        let timeout = std::time::Duration::from_secs(30);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(McpError::ConnectionClosed),
            Err(_) => {
                self.pending.remove(&id);
                Err(McpError::Timeout(timeout))
            }
        }
    }

    async fn send_notification(&self, notification: JsonRpcRequest) -> Result<(), McpError> {
        let line = serde_json::to_string(&notification)?;
        let mut writer = self.writer.lock().await;
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(|e| McpError::Transport(format!("write failed: {}", e)))?;
        writer
            .write_all(b"\n")
            .await
            .map_err(|e| McpError::Transport(format!("write newline failed: {}", e)))?;
        writer
            .flush()
            .await
            .map_err(|e| McpError::Transport(format!("flush failed: {}", e)))?;
        Ok(())
    }

    async fn close(&self) -> Result<(), McpError> {
        let mut child = self.child.lock().await;
        if let Some(mut c) = child.take() {
            let _ = c.kill().await;
        }
        Ok(())
    }
}

impl std::fmt::Debug for StdioTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StdioTransport")
            .field("pending_count", &self.pending.len())
            .finish()
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        // Ensure the child process is killed when the transport is dropped,
        // even if the caller forgot to call close().  This prevents orphaned
        // MCP server processes that leak PIDs and file descriptors.
        //
        // `try_lock()` is safe in Drop — we must not block.
        // `start_kill()` is non-async and sends SIGKILL on Unix.
        if let Ok(mut guard) = self.child.try_lock() {
            if let Some(ref mut child) = *guard {
                let _ = child.start_kill();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SSE Transport
// ---------------------------------------------------------------------------

/// SSE transport — HTTP POST for requests, Server-Sent Events for responses.
pub struct SseTransport {
    /// SSE endpoint URL
    url: String,
    /// HTTP client
    http: reqwest::Client,
    /// Monotonic request ID counter
    next_id: AtomicU64,
}

impl SseTransport {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            http: reqwest::Client::new(),
            next_id: AtomicU64::new(1),
        }
    }

    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

#[async_trait]
impl McpTransport for SseTransport {
    async fn send_request(&self, mut request: JsonRpcRequest) -> Result<JsonRpcResponse, McpError> {
        let id = self.next_request_id();
        request.id = Some(serde_json::Value::Number(id.into()));

        let response = self
            .http
            .post(&self.url)
            .json(&request)
            .send()
            .await
            .map_err(|e| McpError::Transport(format!("HTTP POST failed: {}", e)))?;

        if !response.status().is_success() {
            return Err(McpError::Transport(format!(
                "HTTP {} from MCP server",
                response.status()
            )));
        }

        let body = response
            .text()
            .await
            .map_err(|e| McpError::Transport(format!("read response body: {}", e)))?;

        serde_json::from_str(&body).map_err(|e| McpError::Protocol(format!("parse response: {}", e)))
    }

    async fn send_notification(&self, notification: JsonRpcRequest) -> Result<(), McpError> {
        self.http
            .post(&self.url)
            .json(&notification)
            .send()
            .await
            .map_err(|e| McpError::Transport(format!("HTTP POST notification: {}", e)))?;
        Ok(())
    }

    async fn close(&self) -> Result<(), McpError> {
        Ok(()) // HTTP is stateless
    }
}

impl std::fmt::Debug for SseTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseTransport")
            .field("url", &self.url)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Streamable HTTP Transport (MCP 2025-03-26 spec)
// ---------------------------------------------------------------------------

/// Configuration for the Streamable HTTP transport.
#[derive(Debug, Clone)]
pub struct StreamableHttpConfig {
    /// Base URL of the MCP server (must end in `/mcp`).
    pub url: String,
    /// Optional bearer token or custom headers for authentication.
    pub auth_headers: HashMap<String, String>,
    /// Request timeout.
    pub timeout: std::time::Duration,
    /// Maximum number of retries on transient errors.
    pub max_retries: u32,
    /// Whether to accept SSE streaming responses.
    pub accept_sse: bool,
}

impl Default for StreamableHttpConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            auth_headers: HashMap::new(),
            timeout: std::time::Duration::from_secs(60),
            max_retries: 2,
            accept_sse: true,
        }
    }
}

/// Streamable HTTP transport — the modern MCP transport (spec 2025-03-26).
///
/// Uses HTTP POST for both requests and notifications. Supports:
/// - JSON response bodies for simple request-response
/// - SSE response bodies for streaming (tool progress, partial results)
/// - Session management via `Mcp-Session-Id` header
///
/// This replaces the older SSE transport pattern.
pub struct StreamableHttpTransport {
    config: StreamableHttpConfig,
    http: reqwest::Client,
    next_id: AtomicU64,
    /// Session ID returned by server after initialize.
    session_id: Mutex<Option<String>>,
}

impl StreamableHttpTransport {
    pub fn new(config: StreamableHttpConfig) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(config.timeout)
                .build()
                .unwrap_or_default(),
            config,
            next_id: AtomicU64::new(1),
            session_id: Mutex::new(None),
        }
    }

    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Build the request with common headers.
    fn build_request(&self, body: &JsonRpcRequest) -> Result<reqwest::RequestBuilder, McpError> {
        let mut builder = self
            .http
            .post(&self.config.url)
            .header("Content-Type", "application/json");

        // Accept header: prefer JSON, optionally accept SSE
        if self.config.accept_sse {
            builder = builder.header("Accept", "application/json, text/event-stream");
        } else {
            builder = builder.header("Accept", "application/json");
        }

        // Auth headers
        for (k, v) in &self.config.auth_headers {
            builder = builder.header(k.as_str(), v.as_str());
        }

        // Session header if we have one
        // Safe: we only read session_id in async context
        // We use try_lock to avoid blocking — if locked, skip the header
        if let Ok(guard) = self.session_id.try_lock() {
            if let Some(ref sid) = *guard {
                builder = builder.header("Mcp-Session-Id", sid.as_str());
            }
        }

        Ok(builder.json(body))
    }

    /// Extract and store session ID from response headers.
    async fn capture_session_id(&self, headers: &reqwest::header::HeaderMap) {
        if let Some(val) = headers.get("mcp-session-id") {
            if let Ok(sid) = val.to_str() {
                let mut guard = self.session_id.lock().await;
                *guard = Some(sid.to_string());
                debug!(session_id = sid, "MCP session established");
            }
        }
    }

    /// Send with retry on transient failures.
    async fn send_with_retry(
        &self,
        request: &JsonRpcRequest,
    ) -> Result<JsonRpcResponse, McpError> {
        let mut last_err = McpError::Transport("no attempts made".into());

        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                let backoff = std::time::Duration::from_millis(100 * 2u64.pow(attempt - 1));
                tokio::time::sleep(backoff).await;
            }

            let builder = self.build_request(request)?;
            match builder.send().await {
                Ok(resp) => {
                    self.capture_session_id(resp.headers()).await;

                    let status = resp.status();
                    if status.is_success() {
                        let body = resp.text().await.map_err(|e| {
                            McpError::Transport(format!("read response body: {}", e))
                        })?;
                        return serde_json::from_str(&body).map_err(|e| {
                            McpError::Protocol(format!("parse response: {}", e))
                        });
                    }

                    // Non-retryable client errors
                    if status.is_client_error() {
                        return Err(McpError::Transport(format!(
                            "HTTP {} from MCP server",
                            status
                        )));
                    }

                    // Server error — retryable
                    last_err =
                        McpError::Transport(format!("HTTP {} (attempt {})", status, attempt + 1));
                }
                Err(e) if e.is_timeout() => {
                    last_err = McpError::Timeout(self.config.timeout);
                }
                Err(e) if e.is_connect() => {
                    last_err =
                        McpError::Transport(format!("connection failed (attempt {}): {}", attempt + 1, e));
                }
                Err(e) => {
                    return Err(McpError::Transport(format!("HTTP error: {}", e)));
                }
            }
        }

        Err(last_err)
    }
}

#[async_trait]
impl McpTransport for StreamableHttpTransport {
    async fn send_request(&self, mut request: JsonRpcRequest) -> Result<JsonRpcResponse, McpError> {
        let id = self.next_request_id();
        request.id = Some(serde_json::Value::Number(id.into()));
        self.send_with_retry(&request).await
    }

    async fn send_notification(&self, notification: JsonRpcRequest) -> Result<(), McpError> {
        let builder = self.build_request(&notification)?;
        let resp = builder.send().await.map_err(|e| {
            McpError::Transport(format!("notification POST failed: {}", e))
        })?;
        self.capture_session_id(resp.headers()).await;

        // Server may return 202 Accepted or 200 for notifications
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(McpError::Transport(format!(
                "notification HTTP {}",
                resp.status()
            )))
        }
    }

    async fn close(&self) -> Result<(), McpError> {
        // Send DELETE to terminate session if we have a session ID
        let session_id = {
            let guard = self.session_id.lock().await;
            guard.clone()
        };

        if let Some(ref sid) = session_id {
            let mut builder = self.http.delete(&self.config.url);
            builder = builder.header("Mcp-Session-Id", sid.as_str());
            for (k, v) in &self.config.auth_headers {
                builder = builder.header(k.as_str(), v.as_str());
            }
            // Best-effort; don't fail on close errors
            let _ = builder.send().await;
            debug!(session_id = %sid, "MCP session terminated");
        }

        Ok(())
    }
}

impl std::fmt::Debug for StreamableHttpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamableHttpTransport")
            .field("url", &self.config.url)
            .field("accept_sse", &self.config.accept_sse)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_rpc_request_serialization() {
        let req = JsonRpcRequest::new(1, "tools/list", None);
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"method\":\"tools/list\""));
    }

    #[test]
    fn json_rpc_notification_has_no_id() {
        let notif = JsonRpcRequest::notification("notifications/initialized", None);
        let json = serde_json::to_string(&notif).unwrap();
        assert!(json.contains("\"id\":null"));
    }

    #[test]
    fn streamable_http_config_defaults() {
        let config = StreamableHttpConfig::default();
        assert_eq!(config.timeout, std::time::Duration::from_secs(60));
        assert_eq!(config.max_retries, 2);
        assert!(config.accept_sse);
        assert!(config.auth_headers.is_empty());
    }

    #[test]
    fn streamable_http_transport_debug() {
        let config = StreamableHttpConfig {
            url: "http://localhost:3000/mcp".into(),
            ..Default::default()
        };
        let transport = StreamableHttpTransport::new(config);
        let dbg = format!("{:?}", transport);
        assert!(dbg.contains("localhost:3000/mcp"));
        assert!(dbg.contains("accept_sse: true"));
    }
}
