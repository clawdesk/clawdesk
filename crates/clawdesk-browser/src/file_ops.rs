//! File operations — upload, download tracking, and console capture.
//!
//! ## File upload
//! Uses CDP `DOM.setFileInputFiles` to programmatically set files on
//! `<input type="file">` elements.
//!
//! ## Download tracking
//! Uses CDP `Browser.setDownloadBehavior` + `Page.downloadWillBegin` +
//! `Page.downloadProgress` events to track downloads.
//!
//! ## Console capture
//! Uses CDP `Runtime.consoleAPICalled` events to capture console.log/warn/error.

use crate::cdp::CdpSession;
use crate::manager::ConsoleEntry;
use serde::{Deserialize, Serialize};
use tracing::debug;

// ═══════════════════════════════════════════════════════════════
// File Upload
// ═══════════════════════════════════════════════════════════════

/// Upload a file to a `<input type="file">` element.
///
/// The element is identified by CSS selector. The file path must be an
/// absolute path accessible by the Chrome process.
pub async fn upload_file(
    cdp: &CdpSession,
    selector: &str,
    file_paths: &[String],
) -> Result<String, String> {
    // First, find the element's node via DOM
    let find_js = format!(
        r#"(() => {{
            const el = document.querySelector('{}');
            if (!el) return null;
            if (el.tagName !== 'INPUT' || el.type !== 'file') return 'not_file_input';
            return 'found';
        }})()"#,
        selector.replace('\'', "\\'")
    );

    let check = cdp.eval(&find_js).await?;
    let status = check
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("null");

    match status {
        "null" => return Err("file input element not found".into()),
        "not_file_input" => return Err("element is not a file input".into()),
        _ => {}
    }

    // Get the DOM node ID
    let doc_cmd = cdp.build_command("DOM.getDocument", serde_json::json!({"depth": 0}));
    let doc_resp = cdp.send(doc_cmd).await?;
    let root_node_id = doc_resp
        .result
        .as_ref()
        .and_then(|r| r.get("root"))
        .and_then(|r| r.get("nodeId"))
        .and_then(|n| n.as_u64())
        .ok_or("could not get document root node")?;

    let qs_cmd = cdp.build_command(
        "DOM.querySelector",
        serde_json::json!({
            "nodeId": root_node_id,
            "selector": selector,
        }),
    );
    let qs_resp = cdp.send(qs_cmd).await?;
    let node_id = qs_resp
        .result
        .as_ref()
        .and_then(|r| r.get("nodeId"))
        .and_then(|n| n.as_u64())
        .ok_or("could not find file input node")?;

    // Set the files
    let cmd = cdp.build_command(
        "DOM.setFileInputFiles",
        serde_json::json!({
            "nodeId": node_id,
            "files": file_paths,
        }),
    );
    cdp.send(cmd).await?;

    debug!(
        selector,
        files = file_paths.len(),
        "uploaded files to input"
    );

    Ok(format!(
        "Uploaded {} file(s) to {}",
        file_paths.len(),
        selector
    ))
}

// ═══════════════════════════════════════════════════════════════
// Download Behavior
// ═══════════════════════════════════════════════════════════════

/// Download tracking state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadInfo {
    /// Unique download GUID.
    pub guid: String,
    /// Suggested filename.
    pub suggested_filename: String,
    /// Download URL.
    pub url: String,
    /// Current state.
    pub state: DownloadState,
    /// Bytes received so far.
    pub received_bytes: u64,
    /// Total bytes (if known).
    pub total_bytes: Option<u64>,
}

/// Download state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadState {
    InProgress,
    Completed,
    Canceled,
}

/// Enable downloads to a specific directory.
///
/// Chrome will save downloads to `download_path` without showing the save dialog.
pub async fn enable_downloads(cdp: &CdpSession, download_path: &str) -> Result<(), String> {
    let cmd = cdp.build_command(
        "Browser.setDownloadBehavior",
        serde_json::json!({
            "behavior": "allowAndName",
            "downloadPath": download_path,
            "eventsEnabled": true,
        }),
    );
    cdp.send(cmd).await?;
    debug!(path = download_path, "download behavior enabled");
    Ok(())
}

// ═══════════════════════════════════════════════════════════════
// Console Capture
// ═══════════════════════════════════════════════════════════════

/// Enable console capture via CDP Runtime.consoleAPICalled events.
///
/// Call this after connecting to start capturing console output.
/// Messages are collected by polling `drain_console_events`.
pub async fn enable_console_capture(cdp: &CdpSession) -> Result<(), String> {
    // Runtime.enable is needed for consoleAPICalled events
    let cmd = cdp.build_command("Runtime.enable", serde_json::json!({}));
    cdp.send(cmd).await?;
    debug!("console capture enabled");
    Ok(())
}

/// Capture console output by evaluating a JS shim that intercepts console methods.
///
/// This injects a buffer that collects console.log/warn/error/info/debug calls.
/// Call `drain_console_buffer` to retrieve and clear the buffer.
pub async fn inject_console_shim(cdp: &CdpSession) -> Result<(), String> {
    let js = r#"(() => {
        if (window.__clawdesk_console) return;
        window.__clawdesk_console = [];
        const maxEntries = 200;
        const orig = {};
        ['log', 'warn', 'error', 'info', 'debug'].forEach(level => {
            orig[level] = console[level];
            console[level] = (...args) => {
                if (window.__clawdesk_console.length < maxEntries) {
                    window.__clawdesk_console.push({
                        level: level,
                        text: args.map(a => {
                            try { return typeof a === 'object' ? JSON.stringify(a) : String(a); }
                            catch { return String(a); }
                        }).join(' '),
                        timestamp: Date.now()
                    });
                }
                orig[level].apply(console, args);
            };
        });
        // Also capture uncaught errors
        window.addEventListener('error', (e) => {
            if (window.__clawdesk_console.length < maxEntries) {
                window.__clawdesk_console.push({
                    level: 'error',
                    text: e.message + (e.filename ? ' at ' + e.filename + ':' + e.lineno : ''),
                    source: e.filename || undefined,
                    line: e.lineno || undefined,
                    timestamp: Date.now()
                });
            }
        });
        // Capture unhandled promise rejections
        window.addEventListener('unhandledrejection', (e) => {
            if (window.__clawdesk_console.length < maxEntries) {
                window.__clawdesk_console.push({
                    level: 'error',
                    text: 'Unhandled rejection: ' + (e.reason?.message || String(e.reason)),
                    timestamp: Date.now()
                });
            }
        });
    })()"#;

    cdp.eval(js).await?;
    debug!("console capture shim injected");
    Ok(())
}

/// Drain the console capture buffer, returning all collected entries.
///
/// Clears the buffer after reading so subsequent calls get only new messages.
pub async fn drain_console_buffer(cdp: &CdpSession) -> Result<Vec<ConsoleEntry>, String> {
    let js = r#"(() => {
        const buf = window.__clawdesk_console || [];
        window.__clawdesk_console = [];
        return buf;
    })()"#;

    let result = cdp.eval(js).await?;
    let entries_val = result
        .get("result")
        .and_then(|r| r.get("value"))
        .unwrap_or(&result);

    let entries = match entries_val.as_array() {
        Some(arr) => arr
            .iter()
            .map(|e| ConsoleEntry {
                level: e
                    .get("level")
                    .and_then(|v| v.as_str())
                    .unwrap_or("log")
                    .to_string(),
                text: e
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                source: e.get("source").and_then(|v| v.as_str()).map(String::from),
                line: e.get("line").and_then(|v| v.as_u64()).map(|l| l as u32),
                timestamp: e
                    .get("timestamp")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            })
            .collect(),
        None => vec![],
    };

    debug!(count = entries.len(), "drained console buffer");
    Ok(entries)
}

/// Format console entries for LLM display.
pub fn format_console_for_llm(entries: &[ConsoleEntry]) -> String {
    if entries.is_empty() {
        return "No console output.".to_string();
    }

    let mut out = format!("Console output ({} entries):\n", entries.len());

    for entry in entries {
        let icon = match entry.level.as_str() {
            "error" => "❌",
            "warn" | "warning" => "⚠️",
            "info" => "ℹ️",
            "debug" => "🐛",
            _ => "📝",
        };
        out.push_str(&format!(
            "  {} [{}] {}\n",
            icon,
            entry.level,
            truncate_text(&entry.text, 200)
        ));
    }

    out
}

/// Truncate text for display.
fn truncate_text(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        &s[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_console_empty() {
        assert_eq!(format_console_for_llm(&[]), "No console output.");
    }

    #[test]
    fn format_console_entries() {
        let entries = vec![
            ConsoleEntry {
                level: "log".into(),
                text: "Hello".into(),
                source: None,
                line: None,
                timestamp: 0,
            },
            ConsoleEntry {
                level: "error".into(),
                text: "Something failed".into(),
                source: Some("app.js".into()),
                line: Some(42),
                timestamp: 1000,
            },
        ];
        let output = format_console_for_llm(&entries);
        assert!(output.contains("Console output (2 entries)"));
        assert!(output.contains("[log] Hello"));
        assert!(output.contains("[error] Something failed"));
    }

    #[test]
    fn download_state_eq() {
        assert_eq!(DownloadState::InProgress, DownloadState::InProgress);
        assert_ne!(DownloadState::Completed, DownloadState::Canceled);
    }
}
