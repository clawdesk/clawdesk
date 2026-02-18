//! CDP session management — WebSocket-based Chrome DevTools Protocol client.

use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, error, warn};

/// CDP connection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CdpConfig {
    /// WebSocket URL (e.g., ws://127.0.0.1:9222/devtools/page/xxx).
    pub ws_url: String,
    /// Command timeout in milliseconds.
    pub timeout_ms: u64,
    /// Whether to run headless.
    pub headless: bool,
    /// Browser executable path (auto-detected if None).
    pub browser_path: Option<String>,
}

impl Default for CdpConfig {
    fn default() -> Self {
        Self {
            ws_url: String::new(),
            timeout_ms: 30_000,
            headless: true,
            browser_path: None,
        }
    }
}

/// CDP command message.
#[derive(Debug, Clone, Serialize)]
pub struct CdpCommand {
    pub id: u64,
    pub method: String,
    pub params: serde_json::Value,
}

/// CDP response.
#[derive(Debug, Clone, Deserialize)]
pub struct CdpResponse {
    pub id: Option<u64>,
    pub result: Option<serde_json::Value>,
    pub error: Option<CdpError>,
    pub method: Option<String>,
    pub params: Option<serde_json::Value>,
}

/// CDP error.
#[derive(Debug, Clone, Deserialize)]
pub struct CdpError {
    pub code: i64,
    pub message: String,
}

/// Type alias for the WebSocket write half.
type WsWriter = futures::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Message,
>;

/// CDP session handle — manages WebSocket transport and command dispatch.
pub struct CdpSession {
    config: CdpConfig,
    next_id: AtomicU64,
    connected: bool,
    // WebSocket writer (set after connect)
    ws_write: Option<Arc<Mutex<WsWriter>>>,
    // Pending response map (set after connect)
    pending: Option<Arc<Mutex<HashMap<u64, oneshot::Sender<CdpResponse>>>>>,
    // Event receiver for unsolicited CDP events
    event_rx: Option<tokio::sync::Mutex<mpsc::Receiver<CdpResponse>>>,
}

impl CdpSession {
    /// Create a new CDP session (not yet connected).
    pub fn new(config: CdpConfig) -> Self {
        Self {
            config,
            next_id: AtomicU64::new(1),
            connected: false,
            ws_write: None,
            pending: None,
            event_rx: None,
        }
    }

    /// Build a CDP command with auto-incrementing ID.
    pub fn build_command(&self, method: &str, params: serde_json::Value) -> CdpCommand {
        CdpCommand {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            method: method.to_string(),
            params,
        }
    }

    /// Build a navigate command.
    pub fn navigate(&self, url: &str) -> CdpCommand {
        self.build_command("Page.navigate", serde_json::json!({ "url": url }))
    }

    /// Build an evaluate JavaScript command.
    pub fn evaluate(&self, expression: &str) -> CdpCommand {
        self.build_command(
            "Runtime.evaluate",
            serde_json::json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true,
            }),
        )
    }

    /// Build a screenshot command.
    pub fn screenshot(&self, format: &str, quality: Option<u32>) -> CdpCommand {
        let mut params = serde_json::json!({ "format": format });
        if let Some(q) = quality {
            params["quality"] = serde_json::json!(q);
        }
        self.build_command("Page.captureScreenshot", params)
    }

    /// Build a DOM query selector command.
    pub fn query_selector(&self, node_id: u64, selector: &str) -> CdpCommand {
        self.build_command(
            "DOM.querySelector",
            serde_json::json!({
                "nodeId": node_id,
                "selector": selector,
            }),
        )
    }

    /// Build a click command (via JS).
    pub fn click(&self, selector: &str) -> CdpCommand {
        let js = format!(
            "document.querySelector('{}')?.click()",
            selector.replace('\'', "\\'")
        );
        self.evaluate(&js)
    }

    /// Build a type text command (via JS).
    pub fn type_text(&self, selector: &str, text: &str) -> CdpCommand {
        let js = format!(
            "(() => {{ const el = document.querySelector('{}'); if (el) {{ el.value = '{}'; el.dispatchEvent(new Event('input', {{ bubbles: true }})); }} }})()",
            selector.replace('\'', "\\'"),
            text.replace('\'', "\\'")
        );
        self.evaluate(&js)
    }

    /// Build a get page text command.
    pub fn get_text(&self) -> CdpCommand {
        self.evaluate("document.body?.innerText || ''")
    }

    /// Build a get page title command.
    pub fn get_title(&self) -> CdpCommand {
        self.evaluate("document.title")
    }

    /// Build a get page URL command.
    pub fn get_url(&self) -> CdpCommand {
        self.evaluate("window.location.href")
    }

    /// Enable CDP domains.
    pub fn enable_page(&self) -> CdpCommand {
        self.build_command("Page.enable", serde_json::json!({}))
    }

    pub fn enable_dom(&self) -> CdpCommand {
        self.build_command("DOM.enable", serde_json::json!({}))
    }

    pub fn enable_network(&self) -> CdpCommand {
        self.build_command("Network.enable", serde_json::json!({}))
    }

    /// Get the WebSocket URL.
    pub fn ws_url(&self) -> &str {
        &self.config.ws_url
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn config(&self) -> &CdpConfig {
        &self.config
    }

    /// Auto-detect browser executable.
    pub fn detect_browser() -> Option<String> {
        let candidates = if cfg!(target_os = "macos") {
            vec![
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                "/Applications/Chromium.app/Contents/MacOS/Chromium",
                "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            ]
        } else if cfg!(target_os = "linux") {
            vec![
                "/usr/bin/google-chrome",
                "/usr/bin/chromium-browser",
                "/usr/bin/chromium",
                "/usr/bin/microsoft-edge",
            ]
        } else {
            vec![
                "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
                "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
                "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
            ]
        };

        candidates
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
            .map(String::from)
    }

    /// Build command-line args for launching browser in debug mode.
    pub fn browser_launch_args(port: u16, headless: bool, user_data_dir: &str) -> Vec<String> {
        let mut args = vec![
            format!("--remote-debugging-port={}", port),
            format!("--user-data-dir={}", user_data_dir),
            "--no-first-run".to_string(),
            "--no-default-browser-check".to_string(),
            "--disable-background-networking".to_string(),
            "--disable-sync".to_string(),
        ];
        if headless {
            args.push("--headless=new".to_string());
        }
        args
    }

    /// Connect to a Chrome/Chromium instance via WebSocket.
    ///
    /// Establishes a WebSocket connection to the CDP endpoint and spawns
    /// a background reader task that routes responses to waiting callers.
    pub async fn connect(&mut self) -> Result<(), String> {
        if self.config.ws_url.is_empty() {
            return Err("ws_url is empty — set config.ws_url or discover via /json/version".into());
        }

        let (ws_stream, _) = tokio_tungstenite::connect_async(&self.config.ws_url)
            .await
            .map_err(|e| format!("WebSocket connect failed: {}", e))?;

        let (ws_write, mut ws_read) = ws_stream.split();
        let ws_write = Arc::new(Mutex::new(ws_write));

        // Pending response map: id → oneshot sender
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<CdpResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Event channel for unsolicited CDP events
        let (event_tx, event_rx) = mpsc::channel::<CdpResponse>(256);

        // Spawn reader task
        let pending_clone = pending.clone();
        tokio::spawn(async move {
            while let Some(msg_result) = ws_read.next().await {
                match msg_result {
                    Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                        match serde_json::from_str::<CdpResponse>(&text) {
                            Ok(resp) => {
                                if let Some(id) = resp.id {
                                    // Response to a command
                                    let mut map = pending_clone.lock().await;
                                    if let Some(tx) = map.remove(&id) {
                                        let _ = tx.send(resp);
                                    }
                                } else {
                                    // Unsolicited event
                                    let _ = event_tx.send(resp).await;
                                }
                            }
                            Err(e) => {
                                warn!("CDP: failed to parse message: {}", e);
                            }
                        }
                    }
                    Ok(_) => {} // Binary/ping/pong — ignore
                    Err(e) => {
                        error!("CDP WebSocket read error: {}", e);
                        break;
                    }
                }
            }
            debug!("CDP reader task exited");
        });

        self.ws_write = Some(ws_write);
        self.pending = Some(pending);
        self.event_rx = Some(tokio::sync::Mutex::new(event_rx));
        self.connected = true;

        debug!(url = %self.config.ws_url, "CDP connected");
        Ok(())
    }

    /// Send a CDP command and await its response.
    pub async fn send(&self, command: CdpCommand) -> Result<CdpResponse, String> {
        let ws_write = self
            .ws_write
            .as_ref()
            .ok_or("not connected — call connect() first")?;
        let pending = self
            .pending
            .as_ref()
            .ok_or("not connected — call connect() first")?;

        let cmd_id = command.id;
        let json = serde_json::to_string(&command)
            .map_err(|e| format!("serialize command: {}", e))?;

        // Register the pending response before sending
        let (tx, rx) = oneshot::channel();
        {
            let mut map = pending.lock().await;
            map.insert(cmd_id, tx);
        }

        // Send over WebSocket
        {
            let mut writer = ws_write.lock().await;
            writer
                .send(tokio_tungstenite::tungstenite::Message::Text(json))
                .await
                .map_err(|e| format!("WebSocket send: {}", e))?;
        }

        // Await response with timeout
        let timeout = tokio::time::Duration::from_millis(self.config.timeout_ms);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(resp)) => {
                if let Some(ref err) = resp.error {
                    Err(format!("CDP error {}: {}", err.code, err.message))
                } else {
                    Ok(resp)
                }
            }
            Ok(Err(_)) => Err("CDP response channel closed".into()),
            Err(_) => {
                // Clean up pending entry on timeout
                let mut map = pending.lock().await;
                map.remove(&cmd_id);
                Err(format!("CDP command timed out after {}ms", self.config.timeout_ms))
            }
        }
    }

    /// Navigate to a URL and wait for the page to load.
    pub async fn navigate_and_wait(&mut self, url: &str) -> Result<CdpResponse, String> {
        let cmd = self.navigate(url);
        self.send(cmd).await
    }

    /// Execute JavaScript and return the result value.
    pub async fn eval(&self, expression: &str) -> Result<serde_json::Value, String> {
        let cmd = self.evaluate(expression);
        let resp = self.send(cmd).await?;
        Ok(resp.result.unwrap_or(serde_json::Value::Null))
    }

    /// Take a screenshot and return base64-encoded PNG data.
    pub async fn take_screenshot(&self) -> Result<String, String> {
        let cmd = self.screenshot("png", None);
        let resp = self.send(cmd).await?;
        resp.result
            .and_then(|r| r.get("data").and_then(|d| d.as_str().map(String::from)))
            .ok_or_else(|| "no screenshot data in response".into())
    }

    /// Discover the WebSocket debugger URL from Chrome's /json/version endpoint.
    pub async fn discover_ws_url(host: &str, port: u16) -> Result<String, String> {
        let url = format!("http://{}:{}/json/version", host, port);
        let resp = reqwest::get(&url)
            .await
            .map_err(|e| format!("discovery request failed: {}", e))?;
        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("discovery parse failed: {}", e))?;
        data.get("webSocketDebuggerUrl")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| "no webSocketDebuggerUrl in /json/version".into())
    }

    /// Launch a browser process and connect to it.
    pub async fn launch_and_connect(
        config: &CdpConfig,
        port: u16,
    ) -> Result<(std::process::Child, Self), String> {
        let browser_path = config
            .browser_path
            .clone()
            .or_else(Self::detect_browser)
            .ok_or("no browser found — install Chrome, Chromium, or Edge")?;

        let user_data_dir = format!("/tmp/clawdesk-browser-{}", port);
        let args = Self::browser_launch_args(port, config.headless, &user_data_dir);

        let child = std::process::Command::new(&browser_path)
            .args(&args)
            .arg("about:blank")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to launch browser: {}", e))?;

        // Wait for Chrome to start listening
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        let ws_url = Self::discover_ws_url("127.0.0.1", port).await?;
        let mut session = Self::new(CdpConfig {
            ws_url,
            ..config.clone()
        });
        session.connect().await?;

        // Enable required domains
        let _ = session.send(session.enable_page()).await;
        let _ = session.send(session.enable_dom()).await;

        Ok((child, session))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_ids_increment() {
        let session = CdpSession::new(CdpConfig::default());
        let cmd1 = session.navigate("https://example.com");
        let cmd2 = session.get_title();
        assert_eq!(cmd1.id, 1);
        assert_eq!(cmd2.id, 2);
    }

    #[test]
    fn navigate_command_structure() {
        let session = CdpSession::new(CdpConfig::default());
        let cmd = session.navigate("https://example.com");
        assert_eq!(cmd.method, "Page.navigate");
        assert_eq!(cmd.params["url"], "https://example.com");
    }

    #[test]
    fn screenshot_command() {
        let session = CdpSession::new(CdpConfig::default());
        let cmd = session.screenshot("png", Some(80));
        assert_eq!(cmd.method, "Page.captureScreenshot");
        assert_eq!(cmd.params["format"], "png");
        assert_eq!(cmd.params["quality"], 80);
    }

    #[test]
    fn launch_args_headless() {
        let args = CdpSession::browser_launch_args(9222, true, "/tmp/chrome-data");
        assert!(args.iter().any(|a| a.contains("9222")));
        assert!(args.iter().any(|a| a.contains("headless")));
    }

    #[test]
    fn launch_args_headed() {
        let args = CdpSession::browser_launch_args(9222, false, "/tmp/chrome-data");
        assert!(!args.iter().any(|a| a.contains("headless")));
    }
}
