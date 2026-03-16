//! Headless browser automation — manages Chrome/Chromium instances for agent workflows.
//!
//! Supports local Chrome, remote CDP endpoints, and containerized browsers (browserless).
//! CDP proxy bypass: containerized browsers report internal WebSocket URLs that need
//! rewriting to the external Docker/K8s address.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Remote CDP connection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteCdpConfig {
    /// WebSocket URL for the remote CDP endpoint.
    pub ws_url: String,
    /// Whether to apply proxy bypass for containerized browsers.
    pub proxy_bypass: bool,
    /// Internal hostname to replace (e.g., "localhost" inside Docker).
    pub internal_host: Option<String>,
    /// External hostname to use (e.g., "browserless.example.com").
    pub external_host: Option<String>,
}

/// Rewrite a CDP WebSocket URL for containerized browser access.
///
/// Docker/browserless report URLs like `ws://localhost:9222/devtools/page/ABC`
/// but the actual endpoint is `ws://container-host:9222/devtools/page/ABC`.
pub fn rewrite_cdp_url(url: &str, config: &RemoteCdpConfig) -> String {
    if !config.proxy_bypass {
        return url.to_string();
    }
    match (&config.internal_host, &config.external_host) {
        (Some(internal), Some(external)) => url.replace(internal.as_str(), external.as_str()),
        _ => url.to_string(),
    }
}

/// Page interaction action — a single atomic browser operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PageAction {
    Click { selector: String, timeout_ms: Option<u64> },
    Type { selector: String, text: String, delay_ms: Option<u64> },
    Scroll { direction: ScrollDirection, amount: i32 },
    WaitForSelector { selector: String, timeout_ms: u64 },
    WaitForNavigation { timeout_ms: u64 },
    Evaluate { expression: String },
    Screenshot { full_page: bool },
    SelectOption { selector: String, value: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

/// Result of executing a page action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionOutcome {
    pub success: bool,
    pub action_type: String,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    pub duration_ms: u64,
}

/// Batch multiple actions into a single CDP evaluate call.
///
/// Amortizes CDP round-trips: k actions in 1 evaluate = 1×RTT + k×exec_time
/// vs unbatched k×RTT + k×exec_time. For RTT=50ms, batch of 10: 70ms vs 520ms.
#[derive(Debug, Clone)]
pub struct ActionBatch {
    actions: Vec<PageAction>,
    timeout: Duration,
}

impl ActionBatch {
    pub fn new() -> Self {
        Self {
            actions: Vec::new(),
            timeout: Duration::from_secs(30),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn add(&mut self, action: PageAction) {
        self.actions.push(action);
    }

    pub fn len(&self) -> usize {
        self.actions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Generate a JavaScript expression that executes all actions sequentially.
    pub fn to_evaluate_script(&self) -> String {
        let mut parts = Vec::new();
        for (i, action) in self.actions.iter().enumerate() {
            let js = match action {
                PageAction::Click { selector, .. } => {
                    format!(r#"document.querySelector('{}')?.click()"#, escape_js_string(selector))
                }
                PageAction::Type { selector, text, .. } => {
                    format!(
                        r#"(() => {{ const el = document.querySelector('{}'); if (el) {{ el.value = '{}'; el.dispatchEvent(new Event('input', {{bubbles: true}})); }} }})()"#,
                        escape_js_string(selector),
                        escape_js_string(text)
                    )
                }
                PageAction::Scroll { direction, amount } => {
                    let (x, y) = match direction {
                        ScrollDirection::Down => (0, *amount),
                        ScrollDirection::Up => (0, -amount),
                        ScrollDirection::Right => (*amount, 0),
                        ScrollDirection::Left => (-amount, 0),
                    };
                    format!("window.scrollBy({x}, {y})")
                }
                PageAction::Evaluate { expression } => expression.clone(),
                _ => "null".to_string(),
            };
            parts.push(format!("results[{i}] = (() => {{ try {{ return {js}; }} catch(e) {{ return e.message; }} }})()"));
        }
        format!(
            "(() => {{ const results = []; {}; return results; }})()",
            parts.join("; ")
        )
    }
}

impl Default for ActionBatch {
    fn default() -> Self { Self::new() }
}

/// Page action state machine for tracking interaction lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageState {
    Idle,
    Navigating,
    Interacting,
    WaitingForNetwork,
    Error,
}

impl PageState {
    pub fn is_ready(self) -> bool {
        matches!(self, Self::Idle)
    }
}

/// Activity tracker — monitors page activity to determine when it's safe to act.
#[derive(Debug, Clone)]
pub struct ActivityTracker {
    pub state: PageState,
    pub pending_requests: u32,
    pub last_activity_ms: u64,
    /// Consider page idle after this many ms of no network activity.
    pub idle_threshold_ms: u64,
}

impl ActivityTracker {
    pub fn new(idle_threshold_ms: u64) -> Self {
        Self {
            state: PageState::Idle,
            pending_requests: 0,
            last_activity_ms: 0,
            idle_threshold_ms,
        }
    }

    pub fn on_request_start(&mut self) {
        self.pending_requests += 1;
        self.state = PageState::WaitingForNetwork;
    }

    pub fn on_request_end(&mut self) {
        self.pending_requests = self.pending_requests.saturating_sub(1);
        if self.pending_requests == 0 {
            self.state = PageState::Idle;
        }
    }

    pub fn is_idle(&self) -> bool {
        self.state == PageState::Idle && self.pending_requests == 0
    }
}

/// Download interception — captures file downloads triggered by page actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadCapture {
    pub filename: String,
    pub url: String,
    pub mime_type: String,
    pub size_bytes: Option<u64>,
    pub save_path: Option<String>,
}

fn escape_js_string(s: &str) -> String {
    s.replace('\\', "\\\\")
     .replace('\'', "\\'")
     .replace('"', "\\\"")
     .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdp_url_rewrite() {
        let config = RemoteCdpConfig {
            ws_url: "ws://container:9222".into(),
            proxy_bypass: true,
            internal_host: Some("localhost".into()),
            external_host: Some("browserless.example.com".into()),
        };
        let url = "ws://localhost:9222/devtools/page/ABC";
        assert_eq!(
            rewrite_cdp_url(url, &config),
            "ws://browserless.example.com:9222/devtools/page/ABC"
        );
    }

    #[test]
    fn no_bypass_passthrough() {
        let config = RemoteCdpConfig {
            ws_url: "ws://x:9222".into(),
            proxy_bypass: false,
            internal_host: None,
            external_host: None,
        };
        let url = "ws://localhost:9222/devtools/page/X";
        assert_eq!(rewrite_cdp_url(url, &config), url);
    }

    #[test]
    fn action_batch_script() {
        let mut batch = ActionBatch::new();
        batch.add(PageAction::Click { selector: "#btn".into(), timeout_ms: None });
        batch.add(PageAction::Scroll { direction: ScrollDirection::Down, amount: 500 });
        let script = batch.to_evaluate_script();
        assert!(script.contains("querySelector"));
        assert!(script.contains("scrollBy"));
    }

    #[test]
    fn activity_tracker_lifecycle() {
        let mut tracker = ActivityTracker::new(1000);
        assert!(tracker.is_idle());
        tracker.on_request_start();
        assert!(!tracker.is_idle());
        tracker.on_request_end();
        assert!(tracker.is_idle());
    }

    #[test]
    fn batch_empty() {
        let batch = ActionBatch::new();
        assert!(batch.is_empty());
    }
}
