//! Tab management — CDP Target domain for multi-tab browser control.
//!
//! Uses Chrome DevTools Protocol `Target.*` commands to list, open, focus,
//! and close browser tabs. Each tab is a CDP "target" with a unique
//! `targetId`.
//!
//! ## CDP Target domain commands used
//! - `Target.getTargets` — list all targets (pages, iframes, workers)
//! - `Target.createTarget` — open a new tab
//! - `Target.activateTarget` — bring a tab to the foreground
//! - `Target.closeTarget` — close a tab
//! - `Target.attachToTarget` — attach to a target for page-level CDP
//! - `Target.detachFromTarget` — detach from a target

use crate::cdp::CdpSession;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Information about a browser tab (CDP target).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabInfo {
    /// Unique target ID assigned by Chrome.
    pub target_id: String,
    /// Tab title.
    pub title: String,
    /// Tab URL.
    pub url: String,
    /// Target type (e.g., "page", "background_page", "service_worker").
    pub target_type: String,
    /// Whether this tab is currently attached to a CDP session.
    pub attached: bool,
    /// Browser context ID (profile isolation).
    pub browser_context_id: Option<String>,
}

/// Result of creating a new tab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTabResult {
    pub target_id: String,
    pub url: String,
}

/// List all page-type tabs in the browser.
///
/// Filters to only `type == "page"` targets by default.
/// Set `all_types` to true to include service workers, iframes, etc.
pub async fn list_tabs(cdp: &CdpSession, all_types: bool) -> Result<Vec<TabInfo>, String> {
    let cmd = cdp.build_command("Target.getTargets", serde_json::json!({}));
    let resp = cdp.send(cmd).await?;

    let targets = resp
        .result
        .as_ref()
        .and_then(|r| r.get("targetInfos"))
        .and_then(|t| t.as_array())
        .ok_or("no targetInfos in response")?;

    let mut tabs = Vec::new();
    for target in targets {
        let target_type = target
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if !all_types && target_type != "page" {
            continue;
        }

        tabs.push(TabInfo {
            target_id: target
                .get("targetId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            title: target
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            url: target
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            target_type: target_type.to_string(),
            attached: target
                .get("attached")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            browser_context_id: target
                .get("browserContextId")
                .and_then(|v| v.as_str())
                .map(String::from),
        });
    }

    debug!(count = tabs.len(), "listed browser tabs");
    Ok(tabs)
}

/// Open a new tab with an optional URL.
///
/// Returns the new tab's target ID. Defaults to `about:blank` if no URL given.
pub async fn open_tab(cdp: &CdpSession, url: Option<&str>) -> Result<NewTabResult, String> {
    let target_url = url.unwrap_or("about:blank");
    let cmd = cdp.build_command(
        "Target.createTarget",
        serde_json::json!({ "url": target_url }),
    );
    let resp = cdp.send(cmd).await?;

    let target_id = resp
        .result
        .as_ref()
        .and_then(|r| r.get("targetId"))
        .and_then(|v| v.as_str())
        .ok_or("no targetId in createTarget response")?
        .to_string();

    debug!(target_id = %target_id, url = target_url, "opened new tab");

    Ok(NewTabResult {
        target_id,
        url: target_url.to_string(),
    })
}

/// Focus (activate) a tab by its target ID.
///
/// Brings the tab to the foreground in the browser window.
pub async fn focus_tab(cdp: &CdpSession, target_id: &str) -> Result<(), String> {
    let cmd = cdp.build_command(
        "Target.activateTarget",
        serde_json::json!({ "targetId": target_id }),
    );
    cdp.send(cmd).await?;
    debug!(target_id, "focused tab");
    Ok(())
}

/// Close a tab by its target ID.
pub async fn close_tab(cdp: &CdpSession, target_id: &str) -> Result<bool, String> {
    let cmd = cdp.build_command(
        "Target.closeTarget",
        serde_json::json!({ "targetId": target_id }),
    );
    let resp = cdp.send(cmd).await?;

    let success = resp
        .result
        .as_ref()
        .and_then(|r| r.get("success"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    debug!(target_id, success, "closed tab");
    Ok(success)
}

/// Get the WebSocket debugger URL for a specific tab.
///
/// Queries Chrome's `/json` endpoint to find the WebSocket URL for a given
/// target, enabling a separate `CdpSession` to connect to that specific tab.
pub async fn get_tab_ws_url(host: &str, port: u16, target_id: &str) -> Result<String, String> {
    let url = format!("http://{}:{}/json", host, port);
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| format!("tab discovery request failed: {e}"))?;
    let targets: Vec<serde_json::Value> = resp
        .json()
        .await
        .map_err(|e| format!("tab discovery parse failed: {e}"))?;

    for target in &targets {
        let tid = target.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if tid == target_id {
            if let Some(ws) = target.get("webSocketDebuggerUrl").and_then(|v| v.as_str()) {
                return Ok(ws.to_string());
            }
        }
    }

    Err(format!("no WebSocket URL found for target {target_id}"))
}

/// Attach to a target for isolated CDP messaging.
///
/// Returns a session ID that can be used with `Target.sendMessageToTarget`.
/// `flatten` enables protocol flattening so commands go directly to the target.
pub async fn attach_to_target(
    cdp: &CdpSession,
    target_id: &str,
    flatten: bool,
) -> Result<String, String> {
    let cmd = cdp.build_command(
        "Target.attachToTarget",
        serde_json::json!({
            "targetId": target_id,
            "flatten": flatten
        }),
    );
    let resp = cdp.send(cmd).await?;

    let session_id = resp
        .result
        .as_ref()
        .and_then(|r| r.get("sessionId"))
        .and_then(|v| v.as_str())
        .ok_or("no sessionId in attachToTarget response")?
        .to_string();

    debug!(target_id, session_id = %session_id, "attached to target");
    Ok(session_id)
}

/// Detach from a target.
pub async fn detach_from_target(cdp: &CdpSession, session_id: &str) -> Result<(), String> {
    let cmd = cdp.build_command(
        "Target.detachFromTarget",
        serde_json::json!({ "sessionId": session_id }),
    );
    cdp.send(cmd).await?;
    debug!(session_id, "detached from target");
    Ok(())
}

/// Format tab list for LLM display.
pub fn format_tabs_for_llm(tabs: &[TabInfo]) -> String {
    if tabs.is_empty() {
        return "No open tabs.".to_string();
    }

    let mut out = format!("Open tabs ({}):\n", tabs.len());
    for (i, tab) in tabs.iter().enumerate() {
        out.push_str(&format!(
            "  [{}] {} — {}{}\n",
            i,
            tab.title,
            truncate_url(&tab.url, 80),
            if tab.attached { " (active)" } else { "" }
        ));
    }
    out
}

/// Truncate a URL for display.
fn truncate_url(url: &str, max: usize) -> String {
    if url.len() <= max {
        url.to_string()
    } else {
        format!("{}…", &url[..max - 1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_tabs_empty() {
        assert_eq!(format_tabs_for_llm(&[]), "No open tabs.");
    }

    #[test]
    fn format_tabs_display() {
        let tabs = vec![
            TabInfo {
                target_id: "t1".into(),
                title: "Google".into(),
                url: "https://google.com".into(),
                target_type: "page".into(),
                attached: true,
                browser_context_id: None,
            },
            TabInfo {
                target_id: "t2".into(),
                title: "GitHub".into(),
                url: "https://github.com".into(),
                target_type: "page".into(),
                attached: false,
                browser_context_id: None,
            },
        ];
        let output = format_tabs_for_llm(&tabs);
        assert!(output.contains("Open tabs (2)"));
        assert!(output.contains("[0] Google"));
        assert!(output.contains("[1] GitHub"));
        assert!(output.contains("(active)"));
    }

    #[test]
    fn truncate_url_short() {
        assert_eq!(truncate_url("https://a.com", 80), "https://a.com");
    }

    #[test]
    fn truncate_url_long() {
        let long = "https://".to_string() + &"a".repeat(100);
        let truncated = truncate_url(&long, 20);
        assert!(truncated.len() <= 22); // 19 chars + "…" (3 bytes UTF-8)
        assert!(truncated.ends_with('…'));
    }
}
